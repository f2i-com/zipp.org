"""Generate src/unicode_names_table.rs, the `\\N{...}` name table.

    py -3.13 gen_unicode_names.py > src/unicode_names_table.rs

The names: Latin-1, Greek letters, punctuation, currency, arrows,
mathematical operators, box drawing, symbols and dingbats, common emoji and
the control-character aliases (the set ZIPP's parser has always known), plus
the algorithmic CJK UNIFIED IDEOGRAPH-XXXX names over the ranges listed.

Compact form: the distinct words of the names, sorted, one per line; then,
per name in code point order, varint(code point delta), varint(word count)
and each word's varint index. A lookup (only when a program spells `\\N{`)
scans it once.
"""
import sys
import unicodedata

RANGES = [
    (0x0020, 0x007E),    # Basic Latin
    (0x00A0, 0x00FF),    # Latin-1 Supplement
    (0x0391, 0x03C9),    # Greek letters
    (0x2000, 0x206F),    # General Punctuation
    (0x20A0, 0x20C0),    # Currency Symbols
    (0x2100, 0x218B),    # Letterlike Symbols, Number Forms
    (0x2190, 0x22FF),    # Arrows, Mathematical Operators
    (0x2500, 0x27BF),    # Box Drawing .. Dingbats
    (0x3000, 0x3003),    # Ideographic space and punctuation
    (0xFE0E, 0xFE0F),    # Variation selectors 15/16
    (0xFEFF, 0xFEFF),
    (0xFFFD, 0xFFFD),
    (0x1F300, 0x1F64F),  # Pictographs and emoticons
    (0x1F680, 0x1F6FF),  # Transport and map symbols
]
# NameAliases.txt candidates (C0/C1 controls and format characters);
# unicodedata has no alias listing, so each is checked with lookup().
ALIASES = """
NULL NUL; START OF HEADING SOH; START OF TEXT STX; END OF TEXT ETX;
END OF TRANSMISSION EOT; ENQUIRY ENQ; ACKNOWLEDGE ACK; ALERT BEL; BACKSPACE BS;
CHARACTER TABULATION; HORIZONTAL TABULATION; HT; TAB; LINE FEED; NEW LINE;
END OF LINE; LF; NL; EOL; LINE TABULATION; VERTICAL TABULATION; VT; FORM FEED;
FF; CARRIAGE RETURN; CR; SHIFT OUT; LOCKING-SHIFT ONE; SO; SHIFT IN;
LOCKING-SHIFT ZERO; SI; DATA LINK ESCAPE; DLE; DEVICE CONTROL ONE; DC1;
DEVICE CONTROL TWO; DC2; DEVICE CONTROL THREE; DC3; DEVICE CONTROL FOUR; DC4;
NEGATIVE ACKNOWLEDGE; NAK; SYNCHRONOUS IDLE; SYN; END OF TRANSMISSION BLOCK;
ETB; CANCEL; CAN; END OF MEDIUM; EOM; SUBSTITUTE; SUB; ESCAPE; ESC;
INFORMATION SEPARATOR FOUR; FILE SEPARATOR; FS; INFORMATION SEPARATOR THREE;
GROUP SEPARATOR; GS; INFORMATION SEPARATOR TWO; RECORD SEPARATOR; RS;
INFORMATION SEPARATOR ONE; UNIT SEPARATOR; US; SP; DELETE; DEL;
PADDING CHARACTER; PAD; HIGH OCTET PRESET; HOP; BREAK PERMITTED HERE; BPH;
NO BREAK HERE; NBH; INDEX; IND; NEXT LINE; NEL; START OF SELECTED AREA; SSA;
END OF SELECTED AREA; ESA; CHARACTER TABULATION SET; HORIZONTAL TABULATION SET;
HTS; CHARACTER TABULATION WITH JUSTIFICATION;
HORIZONTAL TABULATION WITH JUSTIFICATION; HTJ; LINE TABULATION SET;
VERTICAL TABULATION SET; VTS; PARTIAL LINE FORWARD; PARTIAL LINE DOWN; PLD;
PARTIAL LINE BACKWARD; PARTIAL LINE UP; PLU; REVERSE LINE FEED; REVERSE INDEX;
RI; SINGLE SHIFT TWO; SINGLE-SHIFT-2; SS2; SINGLE SHIFT THREE; SINGLE-SHIFT-3;
SS3; DEVICE CONTROL STRING; DCS; PRIVATE USE ONE; PRIVATE USE-1; PU1;
PRIVATE USE TWO; PRIVATE USE-2; PU2; SET TRANSMIT STATE; STS; CANCEL CHARACTER;
CCH; MESSAGE WAITING; MW; START OF GUARDED AREA; START OF PROTECTED AREA; SPA;
END OF GUARDED AREA; END OF PROTECTED AREA; EPA; START OF STRING; SOS;
SINGLE GRAPHIC CHARACTER INTRODUCER; SGC; SINGLE CHARACTER INTRODUCER; SCI;
CONTROL SEQUENCE INTRODUCER; CSI; STRING TERMINATOR; ST;
OPERATING SYSTEM COMMAND; OSC; PRIVACY MESSAGE; PM;
APPLICATION PROGRAM COMMAND; APC; NBSP; SHY; ZWSP; ZWNJ; ZWJ; LRM; RLM;
BYTE ORDER MARK; BOM; ZWNBSP; VS15; VS16
"""

entries = {}
for lo, hi in RANGES:
    for cp in range(lo, hi + 1):
        name = unicodedata.name(chr(cp), None)
        if name is not None and not name.startswith("CJK UNIFIED IDEOGRAPH-"):
            entries[name] = cp
for group in ALIASES.replace("\n", " ").split(";"):
    words = group.split()
    candidates = [" ".join(words)]
    if len(words) > 1 and len(words[-1]) <= 3 and words[-1] not in ("ONE", "TWO", "SET", "OUT", "IN", "UP"):
        candidates = [" ".join(words[:-1]), words[-1]]
    for alias in candidates:
        try:
            ch = unicodedata.lookup(alias)
        except KeyError:
            sys.stderr.write("not an alias: %s\n" % alias)
            continue
        entries.setdefault(alias, ord(ch))

cjk = []
start = None
for cp in range(0x3400, 0x323B0):
    named = (unicodedata.name(chr(cp), "") == "CJK UNIFIED IDEOGRAPH-%04X" % cp)
    if named and start is None:
        start = cp
    elif not named and start is not None:
        cjk.append((start, cp - 1))
        start = None
if start is not None:
    cjk.append((start, 0x323AF))

lines = sorted(entries.items(), key=lambda kv: (kv[1], kv[0]))
words = sorted({w for name, _ in lines for w in name.split(" ")})
index = {w: i for i, w in enumerate(words)}


def varint(n):
    out = []
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            out.append(b | 0x80)
        else:
            out.append(b)
            return out


data = []
prev = 0
for name, cp in lines:
    data += varint(cp - prev)
    prev = cp
    parts = name.split(" ")
    data += varint(len(parts))
    for w in parts:
        data += varint(index[w])

out = open(sys.stdout.fileno(), "w", encoding="utf-8", newline="\n", closefd=False)
out.write("// Generated from CPython %s's unicodedata (Unicode %s) by gen_unicode_names.py.\n"
          % (sys.version.split()[0], unicodedata.unidata_version))
out.write("// %d names, %d distinct words.\n" % (len(lines), len(words)))
out.write("pub(crate) const WORDS: &str = \"%s\";\n" % "\\n".join(words))
out.write("pub(crate) const ENTRIES: &[u8] = &[\n")
for i in range(0, len(data), 24):
    out.write("    " + ", ".join(str(b) for b in data[i:i + 24]) + ",\n")
out.write("];\n")
out.write("/// `CJK UNIFIED IDEOGRAPH-XXXX` is an algorithmic name over these ranges.\n")
out.write("pub(crate) const CJK_UNIFIED: &[(u32, u32)] = &[\n")
for lo, hi in cjk:
    out.write("    (0x%X, 0x%X),\n" % (lo, hi))
out.write("];\n")
sys.stderr.write("entries=%d words=%d words_bytes=%d entries_bytes=%d\n"
                 % (len(lines), len(words), len("\n".join(words)), len(data)))
