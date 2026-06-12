"use strict";
// real-world bench 3: generate ~1.5MB of markdown (headers, paragraphs,
// nested lists, bold/italic/code spans, links, code fences), render to HTML
// with a hand-written line-based renderer (string ops + an inline-span state
// machine, NO regex). Deterministic.
function mulberry32(seed) {
  var a = seed | 0;
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
function fnv1a(str) {
  var h = 0x811c9dc5;
  for (var i = 0; i < str.length; i++) {
    h = Math.imul(h ^ str.charCodeAt(i), 16777619);
  }
  return h >>> 0;
}
var rnd = mulberry32(0x3A11AD);
function ri(n) { return (rnd() * n) | 0; }
var WORDS = ("the quick brown fox jumps over lazy dog while compilers emit bytecode for nested closures " +
  "and garbage collectors trace heap edges across generations within milliseconds of allocation pressure " +
  "registers spill stack frames inline caches dispatch megamorphic call sites through hidden classes").split(" ");
function word() { return WORDS[ri(WORDS.length)]; }
function span() {
  var r = ri(12);
  if (r === 0) return "**" + word() + " " + word() + "**";
  if (r === 1) return "*" + word() + "*";
  if (r === 2) return "`" + word() + "_" + ri(100) + "`";
  if (r === 3) return "[" + word() + "](https://example.com/" + word() + "/" + ri(1000) + ")";
  return word();
}
function sentence() {
  var n = 6 + ri(12), s = span();
  for (var i = 1; i < n; i++) s += " " + span();
  return s + ".";
}

// ---- generate the document ----
var parts = [], len = 0;
while (len < 1500000) {
  var t = ri(12), block;
  if (t === 0) block = "#".substring(0, 1) + " " + word() + " " + word() + "\n\n";
  else if (t === 1) block = "## " + word() + " " + ri(100) + "\n\n";
  else if (t === 2) block = "### " + word() + " " + word() + " " + word() + "\n\n";
  else if (t < 7) block = sentence() + " " + sentence() + "\n" + sentence() + "\n\n";
  else if (t < 10) {
    block = "";
    var items = 3 + ri(5);
    for (var li = 0; li < items; li++) {
      var depth = ri(3);
      block += "    ".substring(0, depth * 2) + "- " + span() + " " + span() + "\n";
    }
    block += "\n";
  } else {
    block = "```\n";
    var lines = 2 + ri(5);
    for (var ci = 0; ci < lines; ci++) {
      block += "let " + word() + "_" + ri(100) + " = " + word() + "(" + ri(1000) + ") < " + ri(50) + " && " + ri(9) + ";\n";
    }
    block += "```\n\n";
  }
  parts.push(block);
  len += block.length;
}
var md = parts.join("");

// ---- renderer: line-based, no regex ----
function escapeHtml(s) {
  // fast path: scan for chars needing escape
  var out = "", last = 0;
  for (var i = 0; i < s.length; i++) {
    var c = s.charCodeAt(i);
    if (c === 38) { out += s.substring(last, i) + "&amp;"; last = i + 1; }
    else if (c === 60) { out += s.substring(last, i) + "&lt;"; last = i + 1; }
    else if (c === 62) { out += s.substring(last, i) + "&gt;"; last = i + 1; }
  }
  return last === 0 ? s : out + s.substring(last);
}
function renderInline(s) {
  var out = "", i = 0, n = s.length;
  var bold = false, ital = false;
  while (i < n) {
    var c = s.charCodeAt(i);
    if (c === 42) { // '*'
      if (i + 1 < n && s.charCodeAt(i + 1) === 42) {
        out += bold ? "</strong>" : "<strong>";
        bold = !bold;
        i += 2;
      } else {
        out += ital ? "</em>" : "<em>";
        ital = !ital;
        i += 1;
      }
      continue;
    }
    if (c === 96) { // '`' code span: escape contents, no nesting
      var j = i + 1;
      while (j < n && s.charCodeAt(j) !== 96) j++;
      out += "<code>" + escapeHtml(s.substring(i + 1, j)) + "</code>";
      i = j + 1;
      continue;
    }
    if (c === 91) { // '[' link: [text](url)
      var ct = i + 1;
      while (ct < n && s.charCodeAt(ct) !== 93) ct++;
      if (ct + 1 < n && s.charCodeAt(ct + 1) === 40) {
        var cu = ct + 2;
        while (cu < n && s.charCodeAt(cu) !== 41) cu++;
        out += '<a href="' + s.substring(ct + 2, cu) + '">' + escapeHtml(s.substring(i + 1, ct)) + "</a>";
        i = cu + 1;
        continue;
      }
    }
    if (c === 38) { out += "&amp;"; i++; continue; }
    if (c === 60) { out += "&lt;"; i++; continue; }
    if (c === 62) { out += "&gt;"; i++; continue; }
    out += s[i];
    i++;
  }
  return out;
}
function render(src) {
  var lines = src.split("\n");
  var out = [];
  var inCode = false;
  var listDepth = -1; // current open <ul> nesting depth (-1 = none)
  var para = [];
  function flushPara() {
    if (para.length) {
      out.push("<p>" + renderInline(para.join(" ")) + "</p>");
      para.length = 0;
    }
  }
  function closeLists(to) {
    while (listDepth > to) { out.push("</ul>"); listDepth--; }
  }
  for (var li = 0; li < lines.length; li++) {
    var line = lines[li];
    if (inCode) {
      if (line === "```") { out.push("</pre>"); inCode = false; }
      else out.push(escapeHtml(line));
      continue;
    }
    if (line === "```") { flushPara(); closeLists(-1); out.push("<pre>"); inCode = true; continue; }
    if (line.length === 0) { flushPara(); closeLists(-1); continue; }
    var c0 = line.charCodeAt(0);
    if (c0 === 35) { // headers
      var lvl = 0;
      while (lvl < line.length && line.charCodeAt(lvl) === 35) lvl++;
      flushPara(); closeLists(-1);
      out.push("<h" + lvl + ">" + renderInline(line.substring(lvl + 1)) + "</h" + lvl + ">");
      continue;
    }
    // list item? leading spaces then "- "
    var sp = 0;
    while (sp < line.length && line.charCodeAt(sp) === 32) sp++;
    if (sp + 1 < line.length && line.charCodeAt(sp) === 45 && line.charCodeAt(sp + 1) === 32) {
      var depth = sp >> 1;
      flushPara();
      while (listDepth < depth) { out.push("<ul>"); listDepth++; }
      closeLists(depth);
      out.push("<li>" + renderInline(line.substring(sp + 2)) + "</li>");
      continue;
    }
    para.push(line);
  }
  flushPara();
  closeLists(-1);
  return out.join("\n");
}

var ROUNDS = 6, html = "", hacc = 0;
for (var round = 0; round < ROUNDS; round++) {
  html = render(md);
  hacc = (hacc + fnv1a(html)) >>> 0;
}
console.log("mdLen=" + md.length + " htmlLen=" + html.length + " htmlHashAcc=" + hacc);
