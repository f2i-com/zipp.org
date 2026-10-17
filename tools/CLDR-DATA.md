# DateTimeFormat CLDR data

Moved from the former Test262 corrections README, which no longer exists: the
pinned corpus now runs once, unmodified (see `tools/test262-expected-failures.txt`).

The DateTimeFormat modules `cldr_dtf_en.rs`, `cldr_de.rs`, `cldr_ja.rs`,
`cldr_zh.rs` and `cldr_ar_eg.rs` are generated from the official
[CLDR JSON 48.0.0 release](https://github.com/unicode-org/cldr-json/tree/48.0.0),
under the repository's `LICENSE-UNICODE`. Their headers record source hashes.
Download each locale's date/calendar/number/time-zone JSON inputs and supplemental
dayPeriods, timeData and likelySubtags listed by `tools/gen_cldr_en.py`.
Japanese and Chinese also require their `cldr-rbnf` locale `rbnf.json`.
For example, regenerate German with:

```sh
python tools/gen_cldr_en.py target/cldr-dtf-de-48 --version 48.0.0 \
  --locale de --date-time-only -o crates/zipp-vm/src/vm/cldr_de.rs
rustfmt --edition 2021 crates/zipp-vm/src/vm/cldr_de.rs
```

DateTimeFormat advertises `en`/`en-US`, `de`/`de-DE`, `ja`/`ja-JP`,
`zh`/`zh-CN` and `ar-EG`. Its month/weekday/era
names, styles, flexible day periods, interval patterns and UTC long name use
the selected locale. German defaults to a 24-hour clock; explicit hour-cycle
overrides are supported. Calendar-specific patterns preserve related years,
cyclic year names and leap-month names. Other Intl services retain their
English locale set and existing CLDR 47 data.
This adds neither broad locale coverage nor a claim of full Node/ICU parity.
