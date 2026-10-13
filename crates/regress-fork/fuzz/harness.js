// The differential fuzzer's case runner (see fuzz.cjs). It runs in every
// engine compared, so it sticks to plain ES2015 and prints one line per case;
// a string is printed code unit by code unit, so lone surrogates and
// non-ASCII text compare exactly whatever the engine's console does.
// `CASES` is an array of [pattern, flags, subject, lastIndex] set before this;
// with `NO_LOOP` set the host calls `runOne(i)` itself (wasm_runner.cjs).
function enc(s) {
  if (s === undefined) return "U";
  if (typeof s !== "string") return "?" + typeof s;
  if (s.length > 160) {
    // Long strings print as their length and an FNV-1a hash of the code
    // units, so a line stays well under the console's 64 KB per argument.
    var h = 0x811c9dc5;
    for (var j = 0; j < s.length; j++) h = Math.imul(h ^ s.charCodeAt(j), 16777619);
    return "#" + s.length + ":" + (h >>> 0).toString(16);
  }
  var r = '"';
  for (var i = 0; i < s.length; i++) {
    var c = s.charCodeAt(i);
    if (c >= 0x20 && c < 0x7f && c !== 0x5c && c !== 0x22) r += String.fromCharCode(c);
    else r += "\\" + c.toString(16) + ";";
  }
  return r + '"';
}
function encPair(p) {
  return p === undefined ? "U" : "[" + p[0] + "," + p[1] + "]";
}
function encGroups(g, f) {
  if (g === undefined) return "";
  var keys = Object.keys(g), r = "{";
  for (var i = 0; i < keys.length; i++) r += (i ? "," : "") + enc(keys[i]) + ":" + f(g[keys[i]]);
  return r + "}";
}
function encMatch(m) {
  if (m === null) return "null";
  var r = "@" + m.index + "[";
  for (var i = 0; i < m.length; i++) r += (i ? "," : "") + enc(m[i]);
  r += "]";
  if (m.groups !== undefined) r += "g" + encGroups(m.groups, enc);
  if (m.indices !== undefined) {
    r += "d[";
    for (var i = 0; i < m.indices.length; i++) r += (i ? "," : "") + encPair(m.indices[i]);
    r += "]";
    if (m.indices.groups !== undefined) r += encGroups(m.indices.groups, encPair);
  }
  return r;
}
function encList(xs) {
  if (xs === null) return "null";
  var r = "[";
  for (var i = 0; i < xs.length; i++) r += (i ? "," : "") + enc(xs[i]);
  return r + "]";
}
// Error messages go after " || " at the end of the line: they are compared
// between ZIPP builds, while only the error's name is compared with Node.
var msgs = [];
function err(tag, e) {
  msgs.push(enc(String(e && e.message)));
  return tag + " " + (e && e.name);
}
function runCase(c) {
  msgs = [];
  var out = runCase1(c) + literal(c);
  return msgs.length ? out + " || " + msgs.join(" ;; ") : out;
}
// The same pattern as a regular expression literal, through the compiler's
// early-error check, when it can be written as one: no slash, no line
// terminator, no trailing lone backslash.
var LINE_OR_SLASH = new RegExp("[/\\n\\r" + String.fromCharCode(0x2028, 0x2029) + "]");
function literal(c) {
  var p = c[0];
  if (p === "" || LINE_OR_SLASH.test(p) || /\\$/.test(p.replace(/\\\\/g, ""))) return "";
  try {
    var re = (0, eval)("/" + p + "/" + c[1]);
    return " L" + (re.source === p && re.flags.length === c[1].length ? "=" : enc(re.source + "/" + re.flags));
  } catch (e) { return err(" LX", e); }
}
function runCase1(c) {
  var re;
  try { re = new RegExp(c[0], c[1]); } catch (e) { return err("E", e); }
  var out = "";
  re.lastIndex = c[3];
  for (var k = 0; k < 4; k++) {
    var m;
    try { m = re.exec(c[2]); } catch (e) { out += err(" X", e); break; }
    out += " " + encMatch(m) + "/" + re.lastIndex;
    if (m === null || !(re.global || re.sticky)) break;
  }
  try {
    out += " R" + enc(c[2].replace(new RegExp(c[0], c[1]), "<$&|$1>"));
  } catch (e) { out += err(" RX", e); }
  try {
    var parts = c[2].split(new RegExp(c[0], c[1]), 6), p = "";
    for (var i = 0; i < parts.length; i++) p += (i ? "," : "") + enc(parts[i]);
    out += " S[" + p + "]";
  } catch (e) { out += err(" SX", e); }
  try {
    out += " Q" + c[2].search(new RegExp(c[0], c[1]));
  } catch (e) { out += err(" QX", e); }
  try {
    var all = c[2].match(new RegExp(c[0], c[1]));
    out += " M" + (re.global ? encList(all) : (all === null ? "null" : all.index + ":" + all.length));
  } catch (e) { out += err(" MX", e); }
  if (re.global) {
    try {
      var it = c[2].matchAll(new RegExp(c[0], c[1])), n = 0, last = "";
      for (var x of it) { n++; last = encMatch(x); if (n > 50) break; }
      out += " A" + n + last;
    } catch (e) { out += err(" AX", e); }
  }
  return out;
}
function runOne(i) {
  return runCase(CASES[i]);
}
if (typeof NO_LOOP === "undefined") {
  for (var ci = 0; ci < CASES.length; ci++) console.log(ci + ":" + runCase(CASES[ci]));
  console.log("DONE " + CASES.length);
}
