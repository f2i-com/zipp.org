"""General functions for HTML manipulation (`html.escape` and `html.unescape`)."""
import re

_html5 = None

__all__ = ["escape", "unescape"]

_invalid_charrefs = {
    0x00: "\ufffd",
    0x0D: "\r",
    0x80: "\u20ac",
    0x81: "\x81",
    0x82: "\u201a",
    0x83: "\u0192",
    0x84: "\u201e",
    0x85: "\u2026",
    0x86: "\u2020",
    0x87: "\u2021",
    0x88: "\u02c6",
    0x89: "\u2030",
    0x8A: "\u0160",
    0x8B: "\u2039",
    0x8C: "\u0152",
    0x8D: "\x8d",
    0x8E: "\u017d",
    0x8F: "\x8f",
    0x90: "\x90",
    0x91: "\u2018",
    0x92: "\u2019",
    0x93: "\u201c",
    0x94: "\u201d",
    0x95: "\u2022",
    0x96: "\u2013",
    0x97: "\u2014",
    0x98: "\u02dc",
    0x99: "\u2122",
    0x9A: "\u0161",
    0x9B: "\u203a",
    0x9C: "\u0153",
    0x9D: "\x9d",
    0x9E: "\u017e",
    0x9F: "\u0178",
}

_invalid_codepoints = {
    0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007, 0x0008, 0x000B,
    0x000E, 0x000F, 0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015, 0x0016,
    0x0017, 0x0018, 0x0019, 0x001A, 0x001B, 0x001C, 0x001D, 0x001E, 0x001F,
    0x007F, 0xFDD0, 0xFDD1, 0xFDD2, 0xFDD3, 0xFDD4, 0xFDD5, 0xFDD6, 0xFDD7,
    0xFDD8, 0xFDD9, 0xFDDA, 0xFDDB, 0xFDDC, 0xFDDD, 0xFDDE, 0xFDDF, 0xFDE0,
    0xFDE1, 0xFDE2, 0xFDE3, 0xFDE4, 0xFDE5, 0xFDE6, 0xFDE7, 0xFDE8, 0xFDE9,
    0xFDEA, 0xFDEB, 0xFDEC, 0xFDED, 0xFDEE, 0xFDEF, 0xFFFE, 0xFFFF,
    0x1FFFE, 0x1FFFF, 0x2FFFE, 0x2FFFF, 0x3FFFE, 0x3FFFF, 0x4FFFE, 0x4FFFF,
    0x5FFFE, 0x5FFFF, 0x6FFFE, 0x6FFFF, 0x7FFFE, 0x7FFFF, 0x8FFFE, 0x8FFFF,
    0x9FFFE, 0x9FFFF, 0xAFFFE, 0xAFFFF, 0xBFFFE, 0xBFFFF, 0xCFFFE, 0xCFFFF,
    0xDFFFE, 0xDFFFF, 0xEFFFE, 0xEFFFF, 0xFFFFE, 0xFFFFF, 0x10FFFE, 0x10FFFF,
}


def escape(s, quote=True):
    s = s.replace("&", "&amp;")
    s = s.replace("<", "&lt;")
    s = s.replace(">", "&gt;")
    if quote:
        s = s.replace('"', "&quot;")
        s = s.replace("'", "&#x27;")
    return s


def _replace_charref(s):
    s = s.group(1)
    if s[0] == "#":
        if s[1] in "xX":
            num = int(s[2:].rstrip(";"), 16)
        else:
            num = int(s[1:].rstrip(";"))
        if num in _invalid_charrefs:
            return _invalid_charrefs[num]
        if 0xD800 <= num <= 0xDFFF or num > 0x10FFFF:
            return "\ufffd"
        if num in _invalid_codepoints:
            return ""
        return chr(num)
    else:
        global _html5
        if _html5 is None:
            from html.entities import html5 as _h5
            _html5 = _h5
        if s in _html5:
            return _html5[s]
        for x in range(len(s) - 1, 1, -1):
            if s[:x] in _html5:
                return _html5[s[:x]] + s[x:]
        else:
            return "&" + s


_charref = re.compile(r"&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\f <&#;]{1,32};?)")


def unescape(s):
    if "&" not in s:
        return s
    return _charref.sub(_replace_charref, s)
