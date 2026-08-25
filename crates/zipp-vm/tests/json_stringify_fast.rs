//! `JSON.stringify` byte-parity with node across the quote and leaf-emission
//! fast paths (bulk-copied clean runs in `json_quote_into`/`json_quote_wtf8_into`;
//! borrowed string leaves, `fmt_f64_into` numbers and slot-walked object keys
//! in `json_value_into`).
//!
//! EVERY expected line below was produced by executing `SRC` as a script with
//! node v24.12.0 and is asserted byte-for-byte. The set covers: plain ASCII;
//! every escape class (quote, backslash, the \b \f \n \r \t shorthands, bare
//! controls like U+0001 that need a `\u00XX` escape); non-ASCII BMP text
//! (including U+D7FF — the 0xED-lead run boundary — plus U+2028/U+2029, which
//! JSON.stringify does NOT escape); astral pairs; LONE surrogates (isolated,
//! doubled, sandwiched, mixed with pairs — node serializes each as a `\udXXX`
//! escape, and the WTF-8 quoting must match it exactly); long mixed strings;
//! the number grammar edges (0, -0, the 1e21 exponential cutoff, 1e-7,
//! 5e-324, 2^53±1, NaN/Infinity as null); nested objects/arrays with repeated
//! keys; the indent argument (spaces and tab, double-stringified so each case
//! stays one output line); array and function replacers; toJSON; getters; and
//! mutation of the holder DURING serialization (key-snapshot + late
//! value-read semantics).
//!
//! The same lines must also be produced with `ZIPP_NO_JSON_QUOTE_BULK=1` and
//! with `ZIPP_NO_JSON_LEAF_FAST=1` (each restores the corresponding old
//! path) — the switches select implementations, never output.

const SRC: &str = r##"// Control characters and separators are built with String.fromCharCode so this
// source stays printable ASCII + printable UTF-8 (identical semantics in node
// and zipp). Lone surrogates use \uXXXX escapes (they have no literal form).
var C = String.fromCharCode;
// ---- strings: plain ASCII ----
console.log(JSON.stringify(""));
console.log(JSON.stringify("hello world"));
console.log(JSON.stringify("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 ~!@#$%^&*()_+-=[]{};',./<>?"));
// ---- strings: every escape class ----
console.log(JSON.stringify("quote:\" backslash:\\ slash:/"));
console.log(JSON.stringify("\b\f\n\r\t"));
console.log(JSON.stringify(C(0, 1, 2, 7, 11, 14, 31)));
console.log(JSON.stringify("ctrl" + C(1) + "mid" + C(31) + "end" + C(2)));
console.log(JSON.stringify("\"\\\b\f\n\r\t" + C(0) + "x"));
console.log(JSON.stringify(C(31) + " !"));
// ---- strings: non-ASCII BMP ----
console.log(JSON.stringify("héllo wörld ß"));
console.log(JSON.stringify("日本語テスト"));
console.log(JSON.stringify(C(8232) + "line" + C(8233) + "sep" + C(160) + "nb"));
console.log(JSON.stringify(C(55295) + "|" + C(57344) + "|" + C(65533)));
// ---- strings: astral pairs ----
console.log(JSON.stringify("😀"));
console.log(JSON.stringify(C(55296, 56320) + " " + C(56319, 57343)));
console.log(JSON.stringify("\n😀\n"));
// ---- strings: lone surrogates ----
console.log(JSON.stringify("\ud800"));
console.log(JSON.stringify("\udfff"));
console.log(JSON.stringify("\udc00"));
console.log(JSON.stringify("a\ud800b"));
console.log(JSON.stringify("\ud800\ud800"));
console.log(JSON.stringify("\udc00\ud800"));
console.log(JSON.stringify("x\ud83dy\ude00z"));
console.log(JSON.stringify("😀\ud800😀"));
console.log(JSON.stringify("😀k\udfff"));
console.log(JSON.stringify("\\\ud800\""));
console.log(JSON.stringify(C(55296) + C(56320)));
console.log(JSON.stringify(JSON.parse('"\\ud800\\ud801x\\udc37"')));
// ---- strings: long mixed ----
console.log(JSON.stringify("abcdefghij".repeat(100)));
console.log(JSON.stringify(("ab\"c\\d\ne" + C(1) + "fé日😀\ud800_").repeat(40)));
console.log(JSON.stringify("abcdefghij".repeat(50) + "\"" + "klmnopqrst".repeat(50)));
// ---- numbers ----
console.log(JSON.stringify(0));
console.log(JSON.stringify(-0));
console.log(JSON.stringify(1));
console.log(JSON.stringify(-1));
console.log(JSON.stringify(0.5));
console.log(JSON.stringify(-0.5));
console.log(JSON.stringify(1e21));
console.log(JSON.stringify(-1e21));
console.log(JSON.stringify(1e20));
console.log(JSON.stringify(1e-7));
console.log(JSON.stringify(0.000001));
console.log(JSON.stringify(0.0001));
console.log(JSON.stringify(0.000012345678901234567));
console.log(JSON.stringify(5e-324));
console.log(JSON.stringify(2e-308));
console.log(JSON.stringify(9007199254740993));
console.log(JSON.stringify(9007199254740991));
console.log(JSON.stringify(9007199254740992));
console.log(JSON.stringify(NaN));
console.log(JSON.stringify(Infinity));
console.log(JSON.stringify(-Infinity));
console.log(JSON.stringify(0.1));
console.log(JSON.stringify(123456789.123456789));
console.log(JSON.stringify(4660046610375529984));
console.log(JSON.stringify(1.7976931348623157e308));
console.log(JSON.stringify(111111111111111111111));
console.log(JSON.stringify(4294967296));
console.log(JSON.stringify(-2147483648));
console.log(JSON.stringify(1.5e300));
console.log(JSON.stringify([0,-0,1,-1,0.5,1e21,1e-7,5e-324,9007199254740993,NaN,Infinity,-Infinity]));
console.log(JSON.stringify({a:0,b:-0,c:0.5,d:1e21,e:1e-7,f:5e-324,g:9007199254740993,h:NaN,i:-Infinity,j:true,k:false,l:null}));
// ---- objects / arrays, repeated keys, omission, key order ----
console.log(JSON.stringify({k:1,o:{k:2,o:{k:3,a:[{k:4},{k:5},[{k:6}]]}},a:[{k:7}]}));
console.log(JSON.stringify({a:1,b:[2,3],c:"x"}));
console.log(JSON.stringify({k:1,k:2}));
console.log(JSON.stringify({a:undefined,b:function () {},c:1,d:[undefined,function () {},2]}));
console.log(JSON.stringify({"2":"two","1":"one","01":"oh","x":9,"0":"zero"}));
console.log(JSON.stringify({}));
console.log(JSON.stringify([]));
console.log(JSON.stringify({e:{},a:[],n:null}));
console.log(JSON.stringify({"":1}));
console.log(JSON.stringify(undefined));
console.log(JSON.stringify(JSON.parse('{"2":"a","1":"b","01":"c"}')));
// ---- keys needing escapes / non-ASCII keys ----
var ek = {"a\"b":1,"c\\d":2,"e\nf":3,"hé":5,"😀":6,"日":7};
ek[C(1)] = 4;
console.log(JSON.stringify(ek));
// ---- indent argument (double-stringified so each case stays one line) ----
console.log(JSON.stringify(JSON.stringify({a:1,b:[2,{c:"s"},null],d:{e:1.5,f:"g\n"},g:[],h:{}}, null, 2)));
console.log(JSON.stringify(JSON.stringify({a:1,b:[2,{c:"s"}]}, null, "\t")));
console.log(JSON.stringify(JSON.stringify({x:1,y:2.5,z:-0}, null, 4)));
console.log(JSON.stringify(JSON.stringify([1,[2,[3,{}]],"sé",null], null, 2)));
console.log(JSON.stringify(JSON.stringify({a:undefined,b:1,c:{d:undefined},e:[undefined]}, null, 2)));
console.log(JSON.stringify(JSON.stringify({only:{nested:undefined}}, null, 2)));
// ---- replacer forms ----
console.log(JSON.stringify({a:1,b:2,c:{a:3,d:4,b:[5]}}, ["a","c","b"]));
console.log(JSON.stringify({a:1,b:"x",c:[1,2]}, function (k, v) { return typeof v === "number" ? v + 1 : v; }));
// ---- toJSON / getters ----
console.log(JSON.stringify({d:{toJSON:function () { return [1,"two",{t:3}]; }},x:1}));
console.log(JSON.stringify({get a() { return "got"; }, b:1}));
// ---- mutation during serialization (key snapshot + late value reads) ----
var m = {a:{toJSON:function () { delete m.c; m.d = 9; return "aj"; }}, b:1, c:2, d:3};
console.log(JSON.stringify(m));
var g = {get a() { delete g.b; g.c = "cc"; return 1; }, b:2, c:3};
console.log(JSON.stringify(g));
// ---- boxed primitives ----
console.log(JSON.stringify([new Number(2.5), new String("sé\n"), new Boolean(false), new Number(7)]));
// ---- a json-large-shaped document ----
var doc = {items:[], meta:{count:0, title:"résumé 😀"}};
for (var i = 0; i < 25; i++) {
  doc.items.push({id:i, name:"item-" + i, value:i * 0.5, tags:["t" + i, "ué" + i], flag:i % 2 === 0, note:i % 3 === 0 ? null : "n\"o\\te\n" + i});
}
doc.meta.count = doc.items.length;
console.log(JSON.stringify(doc));
console.log(JSON.stringify(JSON.stringify(doc, null, 2)));
"##;

const EXPECTED: &[&str] = &[
    "\"\"",
    "\"hello world\"",
    "\"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 ~!@#$%^&*()_+-=[]{};',./<>?\"",
    "\"quote:\\\" backslash:\\\\ slash:/\"",
    "\"\\b\\f\\n\\r\\t\"",
    "\"\\u0000\\u0001\\u0002\\u0007\\u000b\\u000e\\u001f\"",
    "\"ctrl\\u0001mid\\u001fend\\u0002\"",
    "\"\\\"\\\\\\b\\f\\n\\r\\t\\u0000x\"",
    "\"\\u001f !\"",
    "\"héllo wörld ß\"",
    "\"日本語テスト\"",
    "\" line sep nb\"",
    "\"퟿||�\"",
    "\"😀\"",
    "\"𐀀 􏿿\"",
    "\"\\n😀\\n\"",
    "\"\\ud800\"",
    "\"\\udfff\"",
    "\"\\udc00\"",
    "\"a\\ud800b\"",
    "\"\\ud800\\ud800\"",
    "\"\\udc00\\ud800\"",
    "\"x\\ud83dy\\ude00z\"",
    "\"😀\\ud800😀\"",
    "\"😀k\\udfff\"",
    "\"\\\\\\ud800\\\"\"",
    "\"𐀀\"",
    "\"\\ud800\\ud801x\\udc37\"",
    "\"abcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghij\"",
    "\"ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_ab\\\"c\\\\d\\ne\\u0001fé日😀\\ud800_\"",
    "\"abcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghijabcdefghij\\\"klmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrstklmnopqrst\"",
    "0",
    "0",
    "1",
    "-1",
    "0.5",
    "-0.5",
    "1e+21",
    "-1e+21",
    "100000000000000000000",
    "1e-7",
    "0.000001",
    "0.0001",
    "0.000012345678901234568",
    "5e-324",
    "2e-308",
    "9007199254740992",
    "9007199254740991",
    "9007199254740992",
    "null",
    "null",
    "null",
    "0.1",
    "123456789.12345679",
    "4660046610375530000",
    "1.7976931348623157e+308",
    "111111111111111110000",
    "4294967296",
    "-2147483648",
    "1.5e+300",
    "[0,0,1,-1,0.5,1e+21,1e-7,5e-324,9007199254740992,null,null,null]",
    "{\"a\":0,\"b\":0,\"c\":0.5,\"d\":1e+21,\"e\":1e-7,\"f\":5e-324,\"g\":9007199254740992,\"h\":null,\"i\":null,\"j\":true,\"k\":false,\"l\":null}",
    "{\"k\":1,\"o\":{\"k\":2,\"o\":{\"k\":3,\"a\":[{\"k\":4},{\"k\":5},[{\"k\":6}]]}},\"a\":[{\"k\":7}]}",
    "{\"a\":1,\"b\":[2,3],\"c\":\"x\"}",
    "{\"k\":2}",
    "{\"c\":1,\"d\":[null,null,2]}",
    "{\"0\":\"zero\",\"1\":\"one\",\"2\":\"two\",\"01\":\"oh\",\"x\":9}",
    "{}",
    "[]",
    "{\"e\":{},\"a\":[],\"n\":null}",
    "{\"\":1}",
    "undefined",
    "{\"1\":\"b\",\"2\":\"a\",\"01\":\"c\"}",
    "{\"a\\\"b\":1,\"c\\\\d\":2,\"e\\nf\":3,\"hé\":5,\"😀\":6,\"日\":7,\"\\u0001\":4}",
    "\"{\\n  \\\"a\\\": 1,\\n  \\\"b\\\": [\\n    2,\\n    {\\n      \\\"c\\\": \\\"s\\\"\\n    },\\n    null\\n  ],\\n  \\\"d\\\": {\\n    \\\"e\\\": 1.5,\\n    \\\"f\\\": \\\"g\\\\n\\\"\\n  },\\n  \\\"g\\\": [],\\n  \\\"h\\\": {}\\n}\"",
    "\"{\\n\\t\\\"a\\\": 1,\\n\\t\\\"b\\\": [\\n\\t\\t2,\\n\\t\\t{\\n\\t\\t\\t\\\"c\\\": \\\"s\\\"\\n\\t\\t}\\n\\t]\\n}\"",
    "\"{\\n    \\\"x\\\": 1,\\n    \\\"y\\\": 2.5,\\n    \\\"z\\\": 0\\n}\"",
    "\"[\\n  1,\\n  [\\n    2,\\n    [\\n      3,\\n      {}\\n    ]\\n  ],\\n  \\\"sé\\\",\\n  null\\n]\"",
    "\"{\\n  \\\"b\\\": 1,\\n  \\\"c\\\": {},\\n  \\\"e\\\": [\\n    null\\n  ]\\n}\"",
    "\"{\\n  \\\"only\\\": {}\\n}\"",
    "{\"a\":1,\"c\":{\"a\":3,\"b\":[5]},\"b\":2}",
    "{\"a\":2,\"b\":\"x\",\"c\":[2,3]}",
    "{\"d\":[1,\"two\",{\"t\":3}],\"x\":1}",
    "{\"a\":\"got\",\"b\":1}",
    "{\"a\":\"aj\",\"b\":1,\"d\":9}",
    "{\"a\":1,\"c\":\"cc\"}",
    "[2.5,\"sé\\n\",false,7]",
    "{\"items\":[{\"id\":0,\"name\":\"item-0\",\"value\":0,\"tags\":[\"t0\",\"ué0\"],\"flag\":true,\"note\":null},{\"id\":1,\"name\":\"item-1\",\"value\":0.5,\"tags\":[\"t1\",\"ué1\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n1\"},{\"id\":2,\"name\":\"item-2\",\"value\":1,\"tags\":[\"t2\",\"ué2\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n2\"},{\"id\":3,\"name\":\"item-3\",\"value\":1.5,\"tags\":[\"t3\",\"ué3\"],\"flag\":false,\"note\":null},{\"id\":4,\"name\":\"item-4\",\"value\":2,\"tags\":[\"t4\",\"ué4\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n4\"},{\"id\":5,\"name\":\"item-5\",\"value\":2.5,\"tags\":[\"t5\",\"ué5\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n5\"},{\"id\":6,\"name\":\"item-6\",\"value\":3,\"tags\":[\"t6\",\"ué6\"],\"flag\":true,\"note\":null},{\"id\":7,\"name\":\"item-7\",\"value\":3.5,\"tags\":[\"t7\",\"ué7\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n7\"},{\"id\":8,\"name\":\"item-8\",\"value\":4,\"tags\":[\"t8\",\"ué8\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n8\"},{\"id\":9,\"name\":\"item-9\",\"value\":4.5,\"tags\":[\"t9\",\"ué9\"],\"flag\":false,\"note\":null},{\"id\":10,\"name\":\"item-10\",\"value\":5,\"tags\":[\"t10\",\"ué10\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n10\"},{\"id\":11,\"name\":\"item-11\",\"value\":5.5,\"tags\":[\"t11\",\"ué11\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n11\"},{\"id\":12,\"name\":\"item-12\",\"value\":6,\"tags\":[\"t12\",\"ué12\"],\"flag\":true,\"note\":null},{\"id\":13,\"name\":\"item-13\",\"value\":6.5,\"tags\":[\"t13\",\"ué13\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n13\"},{\"id\":14,\"name\":\"item-14\",\"value\":7,\"tags\":[\"t14\",\"ué14\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n14\"},{\"id\":15,\"name\":\"item-15\",\"value\":7.5,\"tags\":[\"t15\",\"ué15\"],\"flag\":false,\"note\":null},{\"id\":16,\"name\":\"item-16\",\"value\":8,\"tags\":[\"t16\",\"ué16\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n16\"},{\"id\":17,\"name\":\"item-17\",\"value\":8.5,\"tags\":[\"t17\",\"ué17\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n17\"},{\"id\":18,\"name\":\"item-18\",\"value\":9,\"tags\":[\"t18\",\"ué18\"],\"flag\":true,\"note\":null},{\"id\":19,\"name\":\"item-19\",\"value\":9.5,\"tags\":[\"t19\",\"ué19\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n19\"},{\"id\":20,\"name\":\"item-20\",\"value\":10,\"tags\":[\"t20\",\"ué20\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n20\"},{\"id\":21,\"name\":\"item-21\",\"value\":10.5,\"tags\":[\"t21\",\"ué21\"],\"flag\":false,\"note\":null},{\"id\":22,\"name\":\"item-22\",\"value\":11,\"tags\":[\"t22\",\"ué22\"],\"flag\":true,\"note\":\"n\\\"o\\\\te\\n22\"},{\"id\":23,\"name\":\"item-23\",\"value\":11.5,\"tags\":[\"t23\",\"ué23\"],\"flag\":false,\"note\":\"n\\\"o\\\\te\\n23\"},{\"id\":24,\"name\":\"item-24\",\"value\":12,\"tags\":[\"t24\",\"ué24\"],\"flag\":true,\"note\":null}],\"meta\":{\"count\":25,\"title\":\"résumé 😀\"}}",
    "\"{\\n  \\\"items\\\": [\\n    {\\n      \\\"id\\\": 0,\\n      \\\"name\\\": \\\"item-0\\\",\\n      \\\"value\\\": 0,\\n      \\\"tags\\\": [\\n        \\\"t0\\\",\\n        \\\"ué0\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 1,\\n      \\\"name\\\": \\\"item-1\\\",\\n      \\\"value\\\": 0.5,\\n      \\\"tags\\\": [\\n        \\\"t1\\\",\\n        \\\"ué1\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n1\\\"\\n    },\\n    {\\n      \\\"id\\\": 2,\\n      \\\"name\\\": \\\"item-2\\\",\\n      \\\"value\\\": 1,\\n      \\\"tags\\\": [\\n        \\\"t2\\\",\\n        \\\"ué2\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n2\\\"\\n    },\\n    {\\n      \\\"id\\\": 3,\\n      \\\"name\\\": \\\"item-3\\\",\\n      \\\"value\\\": 1.5,\\n      \\\"tags\\\": [\\n        \\\"t3\\\",\\n        \\\"ué3\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 4,\\n      \\\"name\\\": \\\"item-4\\\",\\n      \\\"value\\\": 2,\\n      \\\"tags\\\": [\\n        \\\"t4\\\",\\n        \\\"ué4\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n4\\\"\\n    },\\n    {\\n      \\\"id\\\": 5,\\n      \\\"name\\\": \\\"item-5\\\",\\n      \\\"value\\\": 2.5,\\n      \\\"tags\\\": [\\n        \\\"t5\\\",\\n        \\\"ué5\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n5\\\"\\n    },\\n    {\\n      \\\"id\\\": 6,\\n      \\\"name\\\": \\\"item-6\\\",\\n      \\\"value\\\": 3,\\n      \\\"tags\\\": [\\n        \\\"t6\\\",\\n        \\\"ué6\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 7,\\n      \\\"name\\\": \\\"item-7\\\",\\n      \\\"value\\\": 3.5,\\n      \\\"tags\\\": [\\n        \\\"t7\\\",\\n        \\\"ué7\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n7\\\"\\n    },\\n    {\\n      \\\"id\\\": 8,\\n      \\\"name\\\": \\\"item-8\\\",\\n      \\\"value\\\": 4,\\n      \\\"tags\\\": [\\n        \\\"t8\\\",\\n        \\\"ué8\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n8\\\"\\n    },\\n    {\\n      \\\"id\\\": 9,\\n      \\\"name\\\": \\\"item-9\\\",\\n      \\\"value\\\": 4.5,\\n      \\\"tags\\\": [\\n        \\\"t9\\\",\\n        \\\"ué9\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 10,\\n      \\\"name\\\": \\\"item-10\\\",\\n      \\\"value\\\": 5,\\n      \\\"tags\\\": [\\n        \\\"t10\\\",\\n        \\\"ué10\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n10\\\"\\n    },\\n    {\\n      \\\"id\\\": 11,\\n      \\\"name\\\": \\\"item-11\\\",\\n      \\\"value\\\": 5.5,\\n      \\\"tags\\\": [\\n        \\\"t11\\\",\\n        \\\"ué11\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n11\\\"\\n    },\\n    {\\n      \\\"id\\\": 12,\\n      \\\"name\\\": \\\"item-12\\\",\\n      \\\"value\\\": 6,\\n      \\\"tags\\\": [\\n        \\\"t12\\\",\\n        \\\"ué12\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 13,\\n      \\\"name\\\": \\\"item-13\\\",\\n      \\\"value\\\": 6.5,\\n      \\\"tags\\\": [\\n        \\\"t13\\\",\\n        \\\"ué13\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n13\\\"\\n    },\\n    {\\n      \\\"id\\\": 14,\\n      \\\"name\\\": \\\"item-14\\\",\\n      \\\"value\\\": 7,\\n      \\\"tags\\\": [\\n        \\\"t14\\\",\\n        \\\"ué14\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n14\\\"\\n    },\\n    {\\n      \\\"id\\\": 15,\\n      \\\"name\\\": \\\"item-15\\\",\\n      \\\"value\\\": 7.5,\\n      \\\"tags\\\": [\\n        \\\"t15\\\",\\n        \\\"ué15\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 16,\\n      \\\"name\\\": \\\"item-16\\\",\\n      \\\"value\\\": 8,\\n      \\\"tags\\\": [\\n        \\\"t16\\\",\\n        \\\"ué16\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n16\\\"\\n    },\\n    {\\n      \\\"id\\\": 17,\\n      \\\"name\\\": \\\"item-17\\\",\\n      \\\"value\\\": 8.5,\\n      \\\"tags\\\": [\\n        \\\"t17\\\",\\n        \\\"ué17\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n17\\\"\\n    },\\n    {\\n      \\\"id\\\": 18,\\n      \\\"name\\\": \\\"item-18\\\",\\n      \\\"value\\\": 9,\\n      \\\"tags\\\": [\\n        \\\"t18\\\",\\n        \\\"ué18\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 19,\\n      \\\"name\\\": \\\"item-19\\\",\\n      \\\"value\\\": 9.5,\\n      \\\"tags\\\": [\\n        \\\"t19\\\",\\n        \\\"ué19\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n19\\\"\\n    },\\n    {\\n      \\\"id\\\": 20,\\n      \\\"name\\\": \\\"item-20\\\",\\n      \\\"value\\\": 10,\\n      \\\"tags\\\": [\\n        \\\"t20\\\",\\n        \\\"ué20\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n20\\\"\\n    },\\n    {\\n      \\\"id\\\": 21,\\n      \\\"name\\\": \\\"item-21\\\",\\n      \\\"value\\\": 10.5,\\n      \\\"tags\\\": [\\n        \\\"t21\\\",\\n        \\\"ué21\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": null\\n    },\\n    {\\n      \\\"id\\\": 22,\\n      \\\"name\\\": \\\"item-22\\\",\\n      \\\"value\\\": 11,\\n      \\\"tags\\\": [\\n        \\\"t22\\\",\\n        \\\"ué22\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n22\\\"\\n    },\\n    {\\n      \\\"id\\\": 23,\\n      \\\"name\\\": \\\"item-23\\\",\\n      \\\"value\\\": 11.5,\\n      \\\"tags\\\": [\\n        \\\"t23\\\",\\n        \\\"ué23\\\"\\n      ],\\n      \\\"flag\\\": false,\\n      \\\"note\\\": \\\"n\\\\\\\"o\\\\\\\\te\\\\n23\\\"\\n    },\\n    {\\n      \\\"id\\\": 24,\\n      \\\"name\\\": \\\"item-24\\\",\\n      \\\"value\\\": 12,\\n      \\\"tags\\\": [\\n        \\\"t24\\\",\\n        \\\"ué24\\\"\\n      ],\\n      \\\"flag\\\": true,\\n      \\\"note\\\": null\\n    }\\n  ],\\n  \\\"meta\\\": {\\n    \\\"count\\\": 25,\\n    \\\"title\\\": \\\"résumé 😀\\\"\\n  }\\n}\"",
];

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

#[test]
fn stringify_matches_node_byte_for_byte() {
    let out = run_ok(SRC);
    assert_eq!(out.len(), EXPECTED.len(), "output line count");
    for (i, (got, want)) in out.iter().zip(EXPECTED.iter()).enumerate() {
        assert_eq!(got, want, "line {i} diverged from node");
    }
}
