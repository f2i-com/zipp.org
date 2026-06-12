"use strict";
// real-world bench 6: generate synthetic log lines (seeded), scan with 5
// regexes: literal level match, ip capture groups, path-normalization replace
// with $1, global matchAll count, alternation+anchors. Prints per-regex
// counts + a hash of the replaced corpus.
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
var rnd = mulberry32(0x10C5CAB);
function ri(n) { return (rnd() * n) | 0; }
function pad2(n) { return n < 10 ? "0" + n : "" + n; }
var LEVELS = ["DEBUG", "INFO", "WARN", "ERROR", "TRACE"];
var METHODS = ["GET", "POST", "PUT", "DELETE", "PATCH"];
var SEGS = ["api", "users", "orders", "items", "static", "v2", "profile", "search", "admin", "assets"];
var UAS = [
  "Mozilla/5.0 (X11; Linux x86_64) Gecko/20100101 Firefox/128.0",
  "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/126.0.0.0",
  "curl/8.5.0",
  "python-requests/2.32",
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) Safari/605.1.15"
];

var NLINES = 150000;
var lines = new Array(NLINES);
for (var i = 0; i < NLINES; i++) {
  var ts = "2026-" + pad2(1 + ri(12)) + "-" + pad2(1 + ri(28)) + "T" +
    pad2(ri(24)) + ":" + pad2(ri(60)) + ":" + pad2(ri(60)) + "." + (100 + ri(900)) + "Z";
  var ip = (1 + ri(254)) + "." + ri(256) + "." + ri(256) + "." + (1 + ri(254));
  var nseg = 2 + ri(4), path = "";
  for (var sj = 0; sj < nseg; sj++) {
    path += (ri(5) === 0 ? "//" : "/") + SEGS[ri(SEGS.length)];
    if (ri(3) === 0) path += "/" + ri(10000);
  }
  lines[i] = ts + " [" + LEVELS[ri(5)] + "] " + ip + " " + METHODS[ri(5)] + " " + path +
    " status=" + (200 + ri(400)) + " bytes=" + ri(100000) + " ms=" + ri(2000) +
    " ua=\"" + UAS[ri(5)] + "\"";
}

// 1) literal level match
var reLevel = /\[ERROR\]/;
var errCount = 0;
for (var i = 0; i < NLINES; i++) if (reLevel.test(lines[i])) errCount++;

// 2) ip capture groups: sum all four octets of every match
var reIp = /(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})/;
var octSum = 0, ipMatches = 0;
for (var i = 0; i < NLINES; i++) {
  var m = reIp.exec(lines[i]);
  if (m) { ipMatches++; octSum = (octSum + (+m[1]) + (+m[2]) + (+m[3]) + (+m[4])) | 0; }
}

// 3) path normalization: collapse duplicate slashes before a segment, keep $1
var reSlash = /\/\/+(\w+)/g;
var replParts = new Array(NLINES), changed = 0;
for (var i = 0; i < NLINES; i++) {
  var r = lines[i].replace(reSlash, "/$1");
  if (r.length !== lines[i].length) changed++;
  replParts[i] = r;
}
var replHash = fnv1a(replParts.join("\n"));

// 4) global matchAll over key=value pairs
var reKv = /([a-z]+)=(\d+)/g;
var kvCount = 0, kvSum = 0;
for (var i = 0; i < NLINES; i++) {
  for (var km of lines[i].matchAll(reKv)) {
    kvCount++;
    kvSum = (kvSum + (+km[2])) | 0;
  }
}

// 5) alternation + anchors: timestamped mutating-method lines ending in a quote
var reAnchor = /^2026-\d\d-\d\dT[0-9:.]+Z \[\w+\] \S+ (?:POST|PUT|DELETE) .*"$/;
var mutCount = 0;
for (var i = 0; i < NLINES; i++) if (reAnchor.test(lines[i])) mutCount++;

console.log("err=" + errCount + " ipMatches=" + ipMatches + " octSum=" + octSum +
  " changed=" + changed + " replHash=" + replHash + " kvCount=" + kvCount +
  " kvSum=" + kvSum + " mut=" + mutCount);
