# Named Unicode escapes (\N{...}) from the frontend's bundled name table:
# letters, punctuation, symbols, emoji, control-character aliases and the
# algorithmic CJK names, spelled in any case, in plain, f- and triple-quoted
# strings. Raw strings and bytes keep the escape as text.

names = [
    "\N{LATIN CAPITAL LETTER A}",
    "\N{latin small letter e with acute}",
    "\N{Latin Small Letter Sharp S}",
    "\N{GREEK CAPITAL LETTER OMEGA}",
    "\N{GREEK SMALL LETTER PI}",
    "\N{EM DASH}",
    "\N{EN DASH}",
    "\N{HORIZONTAL ELLIPSIS}",
    "\N{LEFT DOUBLE QUOTATION MARK}",
    "\N{RIGHT DOUBLE QUOTATION MARK}",
    "\N{BULLET}",
    "\N{EURO SIGN}",
    "\N{POUND SIGN}",
    "\N{YEN SIGN}",
    "\N{COPYRIGHT SIGN}",
    "\N{REGISTERED SIGN}",
    "\N{TRADE MARK SIGN}",
    "\N{DEGREE SIGN}",
    "\N{MICRO SIGN}",
    "\N{PLUS-MINUS SIGN}",
    "\N{MULTIPLICATION SIGN}",
    "\N{DIVISION SIGN}",
    "\N{RIGHTWARDS ARROW}",
    "\N{LEFTWARDS DOUBLE ARROW}",
    "\N{INFINITY}",
    "\N{NOT EQUAL TO}",
    "\N{LESS-THAN OR EQUAL TO}",
    "\N{N-ARY SUMMATION}",
    "\N{SQUARE ROOT}",
    "\N{BOX DRAWINGS LIGHT HORIZONTAL}",
    "\N{FULL BLOCK}",
    "\N{BLACK STAR}",
    "\N{SNOWMAN}",
    "\N{CHECK MARK}",
    "\N{HEAVY CHECK MARK}",
    "\N{BALLOT X}",
    "\N{SNAKE}",
    "\N{GRINNING FACE}",
    "\N{ROCKET}",
    "\N{REPLACEMENT CHARACTER}",
    "\N{IDEOGRAPHIC SPACE}",
    "\N{CJK UNIFIED IDEOGRAPH-4E2D}",
    "\N{CJK UNIFIED IDEOGRAPH-6587}",
]
for text in names:
    print(len(text), hex(ord(text)), text.encode("utf-8"))

aliases = {
    "NUL": "\N{NUL}",
    "NULL": "\N{NULL}",
    "TAB": "\N{TAB}",
    "LINE FEED": "\N{LINE FEED}",
    "LF": "\N{LF}",
    "CR": "\N{CR}",
    "ESC": "\N{ESC}",
    "SP": "\N{SP}",
    "DEL": "\N{DEL}",
    "NBSP": "\N{NBSP}",
    "SHY": "\N{SHY}",
    "ZWJ": "\N{ZWJ}",
    "ZWSP": "\N{ZWSP}",
    "BOM": "\N{BOM}",
    "VS16": "\N{VS16}",
    "BYTE ORDER MARK": "\N{BYTE ORDER MARK}",
}
for alias, char in aliases.items():
    print(alias, hex(ord(char)))

word = "caf\N{LATIN SMALL LETTER E WITH ACUTE}"
print(ascii(word), len(word), word.upper() == "CAF\N{LATIN CAPITAL LETTER E WITH ACUTE}")
temperature = 21.5
print(ascii(f"{temperature:.1f}\N{DEGREE SIGN}C \N{EM DASH} {len(names)} names"))
print(ascii("""\N{BLACK HEART SUIT} and
\N{WHITE SMILING FACE}"""))
print(r"\N{EM DASH}", len(r"\N{EM DASH}"))
print(rb"\N{EM DASH}", len(rb"\N{EM DASH}"))
print("\N{em dash}" == "\N{EM DASH}" == "\u2014" == "\U00002014" == "—")
print(ascii("".join(sorted(set("\N{BULLET}\N{bullet}\N{BuLlEt}")))))
print(ascii("{} \N{RIGHTWARDS ARROW} {}".format("a", "b")))
print(ascii("%s\N{MIDDLE DOT}%s" % (1, 2)))
