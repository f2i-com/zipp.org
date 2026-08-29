#!/usr/bin/env python3
"""Validate that PGO inputs are self-contained and not scored-program clones.

The exact-path and SHA-256 boundary catches literal reuse.  This module adds a
second, deliberately conservative boundary: JavaScript is tokenized with names,
numbers, and string contents normalized, then whole programs and function bodies
are compared by structural token n-grams and long contiguous runs.  It is not a
plagiarism detector; it is a fail-closed guard against the kinds of renamed,
resized control-flow copies that can silently train benchmark-only reducers.
"""

from __future__ import annotations

import argparse
import dataclasses
import difflib
import hashlib
import re
import sys
import unicodedata
from decimal import Decimal, InvalidOperation
from pathlib import Path, PurePosixPath
from typing import Sequence


POLICY_ID = (
    "zipp-pgo-structural-similarity-v1;normalized-js-tokens;10gram;"
    "ngram-evidence>=16;"
    "function-containment<0.78;whole-containment<0.66;"
    "window=96/24@0.82;absolute-run<72;short-run<36-or-0.90;"
    "training-source=ascii-lf;training-template-literal=deny;"
    "training-unicode-escape=deny;training-html-comment=deny;"
    "training-hashbang=deny;training-fnv1a=deny;"
    "training-distinctive-numbers=disjoint;training-numeric-tuples=disjoint;"
    "training-cooked-strings+regex-bodies=disjoint;"
    "training-ambiguous-slash=deny;private-id=atomic"
)
NGRAM_WIDTH = 10
NGRAM_EVIDENCE_MIN = 16
FUNCTION_CONTAINMENT_LIMIT = 0.78
WHOLE_CONTAINMENT_LIMIT = 0.66
WINDOW_CONTAINMENT_LIMIT = 0.82
WINDOW_TOKENS = 96
WINDOW_STRIDE = 24
LOCAL_RUN_MIN = 72
SHORT_RUN_MIN = 36
SHORT_RUN_FRACTION = 0.90
MIN_UNIT_TOKENS = 48
JAVASCRIPT_SUFFIXES = frozenset((".js", ".mjs", ".cjs"))

MODULE_TOKEN = re.compile(rb"(?<![A-Za-z0-9_$])(import|require)(?![A-Za-z0-9_$])")
DYNAMIC_CODE_TOKEN = re.compile(
    rb"(?<![A-Za-z0-9_$])(eval|Function)(?![A-Za-z0-9_$])"
)
TEMPLATE_LITERAL_TOKEN = b"`"
UNICODE_ESCAPE_TOKEN = b"\\u"
HTML_COMMENT_TOKENS = (b"<!--", b"-->")
HASHBANG_TOKEN = b"#!"
FNV1A_TOKENS = (b"16777619", b"0x01000193", b"2166136261", b"0x811c9dc5")
NUMERIC_LITERAL_TOKEN = re.compile(
    r"(?:"
    r"0x[0-9a-f](?:_?[0-9a-f])*n?"
    r"|0b[01](?:_?[01])*n?"
    r"|0o[0-7](?:_?[0-7])*n?"
    r"|[0-9](?:_?[0-9])*n"
    r"|(?:[0-9](?:_?[0-9])*(?:\.(?:[0-9](?:_?[0-9])*)?)?"
    r"|\.[0-9](?:_?[0-9])*)(?:e[+-]?[0-9](?:_?[0-9])*)?"
    r")",
    re.IGNORECASE,
)

KEYWORDS = frozenset(
    "async await break case catch class const continue debugger default delete do "
    "else export extends false finally for function if in instanceof let new null "
    "of return static super switch this throw true try typeof var void while with "
    "yield".split()
)
PUNCTUATORS = tuple(
    sorted(
        (
            ">>>=", "===", "!==", "**=", "&&=", "||=", "??=", "<<=", ">>=", ">>>",
            "=>", "==", "!=", "<=", ">=", "++", "--", "&&", "||", "??", "?.",
            "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "**",
            "...", "{", "}", "(", ")", "[", "]", ";", ",", ".", ":", "?",
            "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "<", ">", "=",
        ),
        key=len,
        reverse=True,
    )
)


class CorpusValidationError(ValueError):
    """The training corpus violates a fail-closed provenance rule."""


@dataclasses.dataclass(frozen=True)
class SourceUnit:
    label: str
    tokens: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class SimilarityFinding:
    training_path: str
    scored_path: str
    training_unit: str
    scored_unit: str
    containment: float
    common_ngrams: int
    smaller_ngram_set: int
    longest_run: int
    smaller_tokens: int

    @property
    def local_fraction(self) -> float:
        return self.longest_run / self.smaller_tokens

    @property
    def violates(self) -> bool:
        window_pair = self.training_unit.startswith("window-") or self.scored_unit.startswith(
            "window-"
        )
        function_pair = self.training_unit != "program" and self.scored_unit != "program"
        if window_pair:
            containment_limit = WINDOW_CONTAINMENT_LIMIT
        elif function_pair:
            containment_limit = FUNCTION_CONTAINMENT_LIMIT
        else:
            containment_limit = WHOLE_CONTAINMENT_LIMIT
        copied_ngrams = (
            self.smaller_ngram_set >= NGRAM_EVIDENCE_MIN
            and self.containment >= containment_limit
        )
        copied_long_run = self.longest_run >= LOCAL_RUN_MIN
        copied_short_unit = (
            self.longest_run >= SHORT_RUN_MIN
            and self.local_fraction >= SHORT_RUN_FRACTION
        )
        return copied_ngrams or copied_long_run or copied_short_unit

    def describe(self) -> str:
        return (
            f"{self.training_path}:{self.training_unit} resembles "
            f"{self.scored_path}:{self.scored_unit} "
            f"(10-gram containment={self.containment:.3f}, "
            f"common={self.common_ngrams}/{self.smaller_ngram_set}, "
            f"longest normalized run={self.longest_run}/{self.smaller_tokens})"
        )


@dataclasses.dataclass(frozen=True)
class ValidationReport:
    training_count: int
    scored_count: int
    compared_unit_pairs: int
    maximum: SimilarityFinding | None


def _is_ident_start(ch: str) -> bool:
    return ch in "_$" or unicodedata.category(ch) in {
        "Lu", "Ll", "Lt", "Lm", "Lo", "Nl",
    }


def _is_ident_continue(ch: str) -> bool:
    return _is_ident_start(ch) or ch in "\u200c\u200d" or unicodedata.category(ch) in {
        "Mn", "Mc", "Nd", "Pc",
    }


def _is_line_terminator(ch: str) -> bool:
    return ch in "\n\r\u2028\u2029"


def _decode_js_string_body(body: str) -> str:
    """Decode ordinary ECMAScript string escapes used by corpus literals."""

    result: list[str] = []
    index = 0
    simple = {
        "b": "\b",
        "f": "\f",
        "n": "\n",
        "r": "\r",
        "t": "\t",
        "v": "\v",
        "0": "\0",
        "'": "'",
        '"': '"',
        "\\": "\\",
    }
    while index < len(body):
        current = body[index]
        if current != "\\":
            result.append(current)
            index += 1
            continue
        index += 1
        if index >= len(body):
            raise CorpusValidationError("unterminated string escape")
        escaped = body[index]
        if escaped in "\n\u2028\u2029":
            index += 1
            continue
        if escaped == "\r":
            index += 2 if index + 1 < len(body) and body[index + 1] == "\n" else 1
            continue
        if escaped in simple:
            result.append(simple[escaped])
            index += 1
            continue
        if escaped == "x" and index + 2 < len(body):
            digits = body[index + 1 : index + 3]
            if re.fullmatch(r"[0-9a-fA-F]{2}", digits):
                result.append(chr(int(digits, 16)))
                index += 3
                continue
        if escaped == "u":
            if index + 1 < len(body) and body[index + 1] == "{":
                close = body.find("}", index + 2)
                digits = body[index + 2 : close] if close >= 0 else ""
                if digits and re.fullmatch(r"[0-9a-fA-F]+", digits):
                    value = int(digits, 16)
                    if value <= 0x10FFFF:
                        result.append(chr(value))
                        index = close + 1
                        continue
            elif index + 4 < len(body):
                digits = body[index + 1 : index + 5]
                if re.fullmatch(r"[0-9a-fA-F]{4}", digits):
                    result.append(chr(int(digits, 16)))
                    index += 5
                    continue
        # ECMAScript non-escape characters cook to the escaped character.
        result.append(escaped)
        index += 1
    return "".join(result)


def normalized_js_tokens(
    source: str,
    *,
    reject_ambiguous_slash: bool = False,
    preserve_numbers: bool = False,
    preserve_literals: bool = False,
) -> tuple[str, ...]:
    """Return conservative structural tokens for ordinary benchmark JavaScript.

    Training validation enables ``reject_ambiguous_slash``.  The structural
    scanner is intentionally not a second JavaScript parser, so it accepts only
    slash contexts it can prove are regexp or division.  Everything else is
    rejected before scanning a possible regexp payload: regexp contents can
    themselves contain comment/string-looking bytes that must never hide later
    executable source.
    """

    tokens: list[str] = []
    i = 0
    size = len(source)
    regex_prefix_tokens = {
        "(", "[", "{", "${", ",", ";", ":", "?", "=", "=>", "!", "~",
        "+", "-", "*", "/", "%", "&", "|", "^", "&&", "||", "??",
        "return", "throw", "case", "delete", "typeof", "void", "new",
        "else", "do", "yield", "await", "in", "of", "instanceof",
    }
    control_heads = {"if", "while", "for", "with", "switch", "catch"}
    contextual_regex_prefixes = {"await", "of", "yield"}
    division_predecessors = {
        "ID", "PRIVATE_ID", "NUM", "STR", "REGEX", "TEMPLATE_END", "]", "++", "--",
        "true", "false", "null", "this", "super",
    }

    def consume_string(quote: str) -> None:
        nonlocal i
        start = i
        i += 1
        while i < size:
            current = source[i]
            if current == "\\":
                i += 1
                if i < size and source[i] == "\r" and i + 1 < size and source[i + 1] == "\n":
                    i += 2
                else:
                    i += 1
                continue
            if current == quote:
                body = source[start + 1 : i]
                i += 1
                tokens.append(
                    "STR:" + _decode_js_string_body(body)
                    if preserve_literals
                    else "STR"
                )
                return
            if _is_line_terminator(current):
                raise CorpusValidationError("newline in string literal")
            i += 1
        raise CorpusValidationError("unterminated string literal")

    def regex_end(start: int) -> int | None:
        end = start + 1
        in_class = False
        while end < size:
            current = source[end]
            if current == "\\":
                if end + 1 >= size or _is_line_terminator(source[end + 1]):
                    return None
                end += 2
                continue
            if current == "[":
                in_class = True
            elif current == "]":
                in_class = False
            elif current == "/" and not in_class:
                end += 1
                while end < size and _is_ident_continue(source[end]):
                    end += 1
                return end
            elif _is_line_terminator(current):
                return None
            end += 1
        return None

    def consume_template() -> None:
        nonlocal i
        tokens.append("TEMPLATE")
        i += 1
        while i < size:
            current = source[i]
            if current == "\\":
                i += 1
                if i < size and source[i] == "\r" and i + 1 < size and source[i + 1] == "\n":
                    i += 2
                else:
                    i += 1
                continue
            if current == "`":
                i += 1
                tokens.append("TEMPLATE_END")
                return
            if source.startswith("${", i):
                tokens.append("${")
                i += 2
                scan(stop_at_template_brace=True)
                tokens.append("}")
                continue
            i += 1
        raise CorpusValidationError("unterminated template literal")

    def scan(*, stop_at_template_brace: bool = False) -> None:
        nonlocal i
        brace_blocks: list[bool] = []
        control_parens: list[bool] = []
        regex_after_control = False
        line_terminator_since_token = False
        while i < size:
            ch = source[i]
            if ch.isspace():
                if _is_line_terminator(ch):
                    line_terminator_since_token = True
                i += 1
                continue
            if source.startswith("//", i):
                i += 2
                while i < size and not _is_line_terminator(source[i]):
                    i += 1
                continue
            if source.startswith("/*", i):
                end = source.find("*/", i + 2)
                if end < 0:
                    raise CorpusValidationError("unterminated block comment")
                if any(_is_line_terminator(item) for item in source[i + 2 : end]):
                    line_terminator_since_token = True
                i = end + 2
                continue
            if ch in "'\"":
                consume_string(ch)
                regex_after_control = False
                line_terminator_since_token = False
                continue
            if ch == "`":
                consume_template()
                regex_after_control = False
                line_terminator_since_token = False
                continue
            if ch == "/" and i + 1 < size and source[i + 1] not in "/*":
                previous = tokens[-1] if tokens else None
                property_keyword = (
                    previous in KEYWORDS
                    and len(tokens) >= 2
                    and tokens[-2] in {".", "?."}
                )
                regexp_prefix = previous in regex_prefix_tokens and not property_keyword
                if (
                    reject_ambiguous_slash
                    and previous in contextual_regex_prefixes
                    and not property_keyword
                ):
                    raise CorpusValidationError(
                        "PGO training input uses an ambiguous regex/division slash"
                    )
                if regex_after_control or previous is None or regexp_prefix:
                    end = regex_end(i)
                    if end is None:
                        raise CorpusValidationError("unterminated regular expression literal")
                    tokens.append(
                        "REGEX:" + source[i:end] if preserve_literals else "REGEX"
                    )
                    i = end
                    regex_after_control = False
                    line_terminator_since_token = False
                    continue
                unambiguous_division = (
                    not line_terminator_since_token
                    and (
                        previous in division_predecessors
                        or property_keyword
                        or (
                            preserve_numbers
                            and previous is not None
                            and NUMERIC_LITERAL_TOKEN.fullmatch(previous) is not None
                        )
                        or (
                            preserve_literals
                            and previous is not None
                            and (
                                previous.startswith("STR:")
                                or previous.startswith("REGEX:")
                            )
                        )
                    )
                )
                if not unambiguous_division:
                    if reject_ambiguous_slash:
                        raise CorpusValidationError(
                            "PGO training input uses an ambiguous regex/division slash"
                        )
                    # For diagnostics/scored-source comparison, preserve the slash
                    # as punctuation.  Training never takes this branch because it
                    # fails above before any regexp payload can be swallowed.
            if ch == "#" and i + 1 < size and _is_ident_start(source[i + 1]):
                end = i + 2
                while end < size and _is_ident_continue(source[end]):
                    end += 1
                # Private IdentifierNames may use keyword spellings (`#return`,
                # `#if`). Keep them atomic so neither regex-prefix nor control-head
                # inference can reinterpret the name.
                tokens.append("PRIVATE_ID")
                i = end
                regex_after_control = False
                line_terminator_since_token = False
                continue
            if _is_ident_start(ch):
                end = i + 1
                while end < size and _is_ident_continue(source[end]):
                    end += 1
                word = source[i:end]
                tokens.append(word if word in KEYWORDS else "ID")
                i = end
                regex_after_control = False
                line_terminator_since_token = False
                continue
            if ch.isdigit() or (
                ch == "." and i + 1 < size and source[i + 1].isdigit()
            ):
                start = i
                numeric = NUMERIC_LITERAL_TOKEN.match(source, i)
                if numeric is None:
                    raise CorpusValidationError("invalid numeric literal")
                end = numeric.end()
                tokens.append(source[start:end].lower() if preserve_numbers else "NUM")
                i = end
                regex_after_control = False
                line_terminator_since_token = False
                continue
            matched = None
            for punctuator in PUNCTUATORS:
                if source.startswith(punctuator, i):
                    matched = punctuator
                    break
            if matched is None:
                # Preserve an unknown token rather than silently erasing structure.
                tokens.append(f"U+{ord(ch):04X}")
                i += 1
                regex_after_control = False
                line_terminator_since_token = False
                continue
            if stop_at_template_brace and matched == "}":
                if not brace_blocks:
                    i += 1
                    return
            previous = tokens[-1] if tokens else None
            if matched == "{":
                brace_blocks.append(True)
            elif matched == "}":
                if brace_blocks:
                    brace_blocks.pop()
                tokens.append(matched)
                i += len(matched)
                # A closing brace can end either a statement block or an
                # expression (function/class/object). Treat the following slash
                # conservatively as division so a regexp guess can never swallow
                # executable source through a later slash.
                regex_after_control = False
                line_terminator_since_token = False
                continue
            if matched == "(":
                property_keyword = (
                    previous in control_heads
                    and len(tokens) >= 2
                    and tokens[-2] in {".", "?."}
                )
                control_parens.append(previous in control_heads and not property_keyword)
            elif matched == ")":
                regex_after_control = control_parens.pop() if control_parens else False
                tokens.append(matched)
                i += len(matched)
                line_terminator_since_token = False
                continue
            tokens.append(matched)
            i += len(matched)
            regex_after_control = False
            line_terminator_since_token = False
        if stop_at_template_brace:
            raise CorpusValidationError("unterminated template interpolation")

    scan()
    return tuple(tokens)


def _integer_literal_value(token: str) -> int | None:
    if not NUMERIC_LITERAL_TOKEN.fullmatch(token):
        return None
    spelling = token.lower().replace("_", "")
    if spelling.endswith("n"):
        spelling = spelling[:-1]
    try:
        if spelling.startswith("0x"):
            return int(spelling[2:], 16)
        if spelling.startswith("0b"):
            return int(spelling[2:], 2)
        if spelling.startswith("0o"):
            return int(spelling[2:], 8)
        decimal = Decimal(spelling)
        if not decimal.is_finite() or decimal != decimal.to_integral_value():
            return None
        return int(decimal)
    except (InvalidOperation, ValueError, OverflowError):
        return None


def _numeric_literals(source: str, *, strict: bool) -> dict[int, str]:
    literals: dict[int, str] = {}
    for token in normalized_js_tokens(
        source,
        reject_ambiguous_slash=strict,
        preserve_numbers=True,
    ):
        value = _integer_literal_value(token)
        if value is not None:
            literals.setdefault(value, token)
    return literals


def _literal_sensitive_inventory(
    source: str, *, strict: bool
) -> tuple[set[str], set[str], set[tuple[str, int, str, int]]]:
    tokens = list(
        normalized_js_tokens(
            source,
            reject_ambiguous_slash=strict,
            preserve_numbers=True,
            preserve_literals=True,
        )
    )
    cooked_strings = {
        token[4:]
        for token in tokens
        if token.startswith("STR:")
        and len(token[4:]) >= 8
        and token[4:] != "use strict"
    }
    regex_bodies: set[str] = set()
    for token in tokens:
        if not token.startswith("REGEX:/"):
            continue
        literal = token[len("REGEX:/") :]
        closing = literal.rfind("/")
        body = literal[:closing] if closing >= 0 else literal
        if len(body) >= 4:
            regex_bodies.add(body)

    events: list[tuple[int, str, int]] = []
    operators = {"+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>", ">>>"}
    for index in range(len(tokens) - 1):
        if tokens[index] not in operators:
            continue
        value = _integer_literal_value(tokens[index + 1])
        if value is not None:
            events.append((index, tokens[index], value))
    numeric_pairs = {
        (left_op, left_value, right_op, right_value)
        for (left_index, left_op, left_value), (
            right_index,
            right_op,
            right_value,
        ) in zip(events, events[1:])
        if right_index - left_index <= 10
        and left_op in {"<<", ">>", ">>>"}
        and right_op in {"<<", ">>", ">>>"}
        and left_value > 0
        and right_value > 0
    }
    return cooked_strings, regex_bodies, numeric_pairs


def _generic_numeric_literal(value: int) -> bool:
    """Allow only small, machine-width, or visibly round shared constants."""

    if value <= 0xFFFF:
        return True
    if value > 0 and (value & (value - 1)) == 0:
        return True
    if value > 0 and ((value + 1) & value) == 0:
        return True
    decimal = str(value).rstrip("0")
    return len(decimal) == 1


def _matching_brace(tokens: Sequence[str], opening: int) -> int | None:
    depth = 0
    for index in range(opening, len(tokens)):
        if tokens[index] == "{":
            depth += 1
        elif tokens[index] == "}":
            depth -= 1
            if depth == 0:
                return index
    return None


def source_units(source: str) -> tuple[SourceUnit, ...]:
    tokens = normalized_js_tokens(source)
    units = [SourceUnit("program", tokens)]
    seen: set[tuple[int, int]] = set()
    ordinal = 0
    for index, token in enumerate(tokens):
        is_function = token == "function"
        is_arrow = token == "=>"
        if not (is_function or is_arrow):
            continue
        try:
            opening = tokens.index("{", index + 1)
        except ValueError:
            continue
        # A declaration/call boundary before the brace means this was not a
        # braced function/arrow body.  Concise arrows are intentionally covered
        # only by the whole-program comparison.
        boundary = tokens[index + 1 : opening]
        if is_function and ";" in boundary:
            continue
        if is_arrow and any(item in boundary for item in (";", "=>")):
            continue
        closing = _matching_brace(tokens, opening)
        if closing is None or (opening, closing) in seen:
            continue
        seen.add((opening, closing))
        unit_tokens = tokens[index : closing + 1]
        if len(unit_tokens) >= MIN_UNIT_TOKENS:
            kind = "function" if is_function else "arrow"
            units.append(SourceUnit(f"{kind}-{ordinal}", tuple(unit_tokens)))
            ordinal += 1
    # Capture class bodies, object methods/literals, and large top-level blocks
    # independently of their surrounding program. This closes the gap where a
    # copied class hierarchy was diluted by unrelated prefix/suffix code.
    braces: list[int] = []
    block_ordinal = 0
    for index, token in enumerate(tokens):
        if token == "{":
            braces.append(index)
        elif token == "}" and braces:
            opening = braces.pop()
            if (opening, index) in seen:
                continue
            seen.add((opening, index))
            unit_tokens = tokens[opening : index + 1]
            if len(unit_tokens) >= MIN_UNIT_TOKENS:
                units.append(SourceUnit(f"block-{block_ordinal}", tuple(unit_tokens)))
                block_ordinal += 1
    return tuple(units)


def _ngrams(tokens: Sequence[str]) -> set[tuple[str, ...]]:
    if len(tokens) < NGRAM_WIDTH:
        return set()
    return {
        tuple(tokens[index : index + NGRAM_WIDTH])
        for index in range(len(tokens) - NGRAM_WIDTH + 1)
    }


def _window_units(tokens: Sequence[str]) -> tuple[SourceUnit, ...]:
    if len(tokens) < WINDOW_TOKENS:
        return ()
    last = len(tokens) - WINDOW_TOKENS
    starts = list(range(0, last + 1, WINDOW_STRIDE))
    if starts[-1] != last:
        starts.append(last)
    return tuple(
        SourceUnit(f"window-{start}", tuple(tokens[start : start + WINDOW_TOKENS]))
        for start in starts
    )


def compare_sources(
    training_path: str,
    training_source: str,
    scored_path: str,
    scored_source: str,
) -> tuple[SimilarityFinding, ...]:
    findings: list[SimilarityFinding] = []
    training_units = source_units(training_source)
    scored_units = source_units(scored_source)

    def compare_pair(training_unit: SourceUnit, scored_unit: SourceUnit) -> None:
        if len(training_unit.tokens) < MIN_UNIT_TOKENS:
            return
        training_ngrams = _ngrams(training_unit.tokens)
        if len(scored_unit.tokens) < MIN_UNIT_TOKENS:
            return
        scored_ngrams = _ngrams(scored_unit.tokens)
        smaller_set = min(len(training_ngrams), len(scored_ngrams))
        if smaller_set == 0:
            return
        common = len(training_ngrams & scored_ngrams)
        matcher = difflib.SequenceMatcher(
            None, training_unit.tokens, scored_unit.tokens, autojunk=False
        )
        longest = matcher.find_longest_match().size
        findings.append(
            SimilarityFinding(
                training_path=training_path,
                scored_path=scored_path,
                training_unit=training_unit.label,
                scored_unit=scored_unit.label,
                containment=common / smaller_set,
                common_ngrams=common,
                smaller_ngram_set=smaller_set,
                longest_run=longest,
                smaller_tokens=min(len(training_unit.tokens), len(scored_unit.tokens)),
            )
        )

    for training_unit in training_units:
        for scored_unit in scored_units:
            compare_pair(training_unit, scored_unit)
    training_program = training_units[0]
    scored_program = scored_units[0]
    for training_window in _window_units(training_program.tokens):
        compare_pair(training_window, scored_program)
    for scored_window in _window_units(scored_program.tokens):
        compare_pair(training_program, scored_window)
    return tuple(findings)


def _resolve_regular(root: Path, raw: str) -> Path:
    pure = PurePosixPath(raw)
    if (
        pure.is_absolute()
        or "\\" in raw
        or raw.startswith("//")
        or (len(raw) >= 2 and raw[0].isalpha() and raw[1] == ":")
        or any(part in ("", ".", "..") for part in pure.parts)
        or raw != pure.as_posix()
    ):
        raise CorpusValidationError(f"non-canonical corpus path: {raw!r}")
    path = root.joinpath(*pure.parts)
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(root)
    except (OSError, ValueError) as exc:
        raise CorpusValidationError(f"corpus path escapes or is missing: {raw}") from exc
    if path.is_symlink() or not resolved.is_file():
        raise CorpusValidationError(f"corpus input must be a regular in-tree file: {raw}")
    return resolved


def validate_corpus(
    *, root: Path, training_paths: Sequence[str], scored_paths: Sequence[str]
) -> ValidationReport:
    root = root.resolve(strict=True)
    if not training_paths or not scored_paths:
        raise CorpusValidationError("training and scored input sets must be non-empty")
    if len(training_paths) != len(set(training_paths)):
        raise CorpusValidationError("duplicate PGO training path")
    if len(scored_paths) != len(set(scored_paths)):
        raise CorpusValidationError("duplicate scored input path")

    training: list[tuple[str, bytes, str]] = []
    training_digests: dict[str, str] = {}
    training_numeric_literals: dict[int, tuple[str, str]] = {}
    training_strings: dict[str, str] = {}
    training_regex_bodies: dict[str, str] = {}
    training_numeric_pairs: dict[tuple[str, int, str, int], str] = {}
    for raw in training_paths:
        if raw.startswith("bench/real/") or raw.startswith("bench/hostile/"):
            raise CorpusValidationError(f"scored publication input cannot train PGO: {raw}")
        content = _resolve_regular(root, raw).read_bytes()
        if any(byte >= 0x80 for byte in content):
            raise CorpusValidationError(
                f"PGO training input must use ASCII source spelling: {raw}"
            )
        if b"\r" in content:
            raise CorpusValidationError(
                f"PGO training input must use LF-only line endings: {raw}"
            )
        if MODULE_TOKEN.search(content):
            raise CorpusValidationError(f"PGO training input is not self-contained: {raw}")
        if DYNAMIC_CODE_TOKEN.search(content):
            raise CorpusValidationError(f"PGO training input uses dynamic code evaluation: {raw}")
        if TEMPLATE_LITERAL_TOKEN in content:
            raise CorpusValidationError(
                f"PGO training input uses template literal syntax: {raw}"
            )
        if UNICODE_ESCAPE_TOKEN in content:
            raise CorpusValidationError(
                f"PGO training input uses a raw Unicode escape: {raw}"
            )
        if any(token in content for token in HTML_COMMENT_TOKENS):
            raise CorpusValidationError(
                f"PGO training input uses Annex-B HTML comment syntax: {raw}"
            )
        if HASHBANG_TOKEN in content:
            raise CorpusValidationError(
                f"PGO training input uses hashbang syntax: {raw}"
            )
        if any(token.lower() in content.lower() for token in FNV1A_TOKENS):
            raise CorpusValidationError(
                f"PGO training input uses a scored FNV-1a checksum constant: {raw}"
            )
        digest = hashlib.sha256(content).hexdigest()
        if digest in training_digests:
            raise CorpusValidationError(
                f"duplicate PGO training bytes: {raw} == {training_digests[digest]}"
            )
        training_digests[digest] = raw
        try:
            source = content.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise CorpusValidationError(f"PGO training input is not UTF-8: {raw}") from exc
        # Reject lexer-ambiguous slash contexts before structural comparison.
        # The strict tokenizer raises immediately at the slash, before regexp
        # payload bytes can masquerade as comments or strings and hide a clone.
        for value, spelling in _numeric_literals(source, strict=True).items():
            if not _generic_numeric_literal(value):
                training_numeric_literals.setdefault(value, (raw, spelling))
        strings, regex_bodies, numeric_pairs = _literal_sensitive_inventory(
            source, strict=True
        )
        for value in strings:
            training_strings.setdefault(value, raw)
        for value in regex_bodies:
            training_regex_bodies.setdefault(value, raw)
        for value in numeric_pairs:
            training_numeric_pairs.setdefault(value, raw)
        training.append((raw, content, source))

    scored: list[tuple[str, str, str | None]] = []
    for raw in scored_paths:
        content = _resolve_regular(root, raw).read_bytes()
        digest = hashlib.sha256(content).hexdigest()
        if digest in training_digests:
            raise CorpusValidationError(
                f"PGO input duplicates scored input bytes: {training_digests[digest]} == {raw}"
            )
        try:
            source = content.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise CorpusValidationError(f"scored input is not UTF-8: {raw}") from exc
        structural_source = (
            source if PurePosixPath(raw).suffix.lower() in JAVASCRIPT_SUFFIXES else None
        )
        if structural_source is not None:
            scored_numbers = _numeric_literals(structural_source, strict=False)
            shared = sorted(set(training_numeric_literals) & set(scored_numbers))
            if shared:
                value = shared[0]
                training_path, training_spelling = training_numeric_literals[value]
                raise CorpusValidationError(
                    "distinctive numeric literal reused across PGO training and "
                    f"scored input: {training_path}:{training_spelling} == "
                    f"{raw}:{scored_numbers[value]} (value={value})"
                )
            strings, regex_bodies, numeric_pairs = _literal_sensitive_inventory(
                structural_source, strict=False
            )
            shared_strings = sorted(set(training_strings) & strings)
            if shared_strings:
                value = shared_strings[0]
                raise CorpusValidationError(
                    "cooked string literal reused across PGO training and scored "
                    f"input: {training_strings[value]} == {raw}: {value!r}"
                )
            shared_regex = sorted(set(training_regex_bodies) & regex_bodies)
            if shared_regex:
                value = shared_regex[0]
                raise CorpusValidationError(
                    "regular-expression body reused across PGO training and scored "
                    f"input: {training_regex_bodies[value]} == {raw}: {value!r}"
                )
            shared_pairs = sorted(set(training_numeric_pairs) & numeric_pairs)
            if shared_pairs:
                value = shared_pairs[0]
                raise CorpusValidationError(
                    "ordered numeric operator tuple reused across PGO training and "
                    f"scored input: {training_numeric_pairs[value]} == {raw}: {value!r}"
                )
        scored.append((raw, digest, structural_source))

    maximum: SimilarityFinding | None = None
    compared = 0
    violations: list[SimilarityFinding] = []
    for training_path, _content, training_source in training:
        for scored_path, _digest, scored_source in scored:
            if scored_source is None:
                continue
            findings = compare_sources(
                training_path, training_source, scored_path, scored_source
            )
            compared += len(findings)
            for finding in findings:
                rank = (finding.containment, finding.local_fraction, finding.longest_run)
                if maximum is None or rank > (
                    maximum.containment,
                    maximum.local_fraction,
                    maximum.longest_run,
                ):
                    maximum = finding
                if finding.violates:
                    violations.append(finding)
    if violations:
        worst = max(
            violations,
            key=lambda item: (item.containment, item.local_fraction, item.longest_run),
        )
        raise CorpusValidationError("structural PGO clone rejected: " + worst.describe())
    return ValidationReport(len(training), len(scored), compared, maximum)


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate = subparsers.add_parser("validate")
    validate.add_argument("--root", type=Path, required=True)
    validate.add_argument("--training", action="append", required=True)
    validate.add_argument("--scored", action="append", required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)
    try:
        report = validate_corpus(
            root=args.root,
            training_paths=args.training,
            scored_paths=args.scored,
        )
    except (CorpusValidationError, OSError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    maximum = report.maximum.describe() if report.maximum else "no comparable units"
    print(
        f"PGO corpus validation passed ({POLICY_ID}): "
        f"{report.training_count} training / {report.scored_count} scored; "
        f"maximum {maximum}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
