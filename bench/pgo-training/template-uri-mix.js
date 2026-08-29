"use strict";

// Independent URI-template expansion.  It uses indexed placeholder lookup and
// piece arrays rather than the scored renderer's line loop, inline-span state
// machine, pending-substring escape accumulator, or HTML output vocabulary.
var seed = 0x4c957f2d;
function next(bound) {
  seed = (seed + 0x2c1b3c6d) | 0;
  seed ^= seed >>> 14;
  seed = ((seed << 11) | (seed >>> 21)) | 0;
  return (seed >>> 0) % bound;
}

var HEX = "0123456789ABCDEF";
function uriSegment(value) {
  var pieces = [];
  for (var i = 0; i < value.length; i++) {
    var code = value.charCodeAt(i);
    var safe = (code >= 48 && code <= 57) || (code >= 65 && code <= 90) ||
      (code >= 97 && code <= 122) || code === 45 || code === 46 || code === 95 || code === 126;
    if (safe) pieces.push(value[i]);
    else pieces.push("%" + HEX[(code >>> 4) & 15] + HEX[code & 15]);
  }
  return pieces.join("");
}

function expand(pattern, values) {
  var output = [];
  var cursor = 0;
  while (cursor < pattern.length) {
    var open = pattern.indexOf("{", cursor);
    if (open < 0) {
      output.push(pattern.substring(cursor));
      break;
    }
    if (open > cursor) output.push(pattern.substring(cursor, open));
    var close = pattern.indexOf("}", open + 1);
    if (close < 0) {
      output.push(pattern.substring(open));
      break;
    }
    var key = pattern.substring(open + 1, close);
    output.push(uriSegment("" + values[key]));
    cursor = close + 1;
  }
  return output.join("");
}

var templates = [
  "/tenant/{tenant}/asset/{asset}?rev={revision}",
  "/region/{region}/report/{report}/{format}",
  "/catalog/{category}/{slug}?page={page}&tag={tag}",
  "/team/{team}/build/{build}/artifact/{artifact}"
];
var words = ["north ridge", "blue/green", "delta+echo", "plain", "salt&pepper", "vivid_sky"];

var REQUESTS = 36000;
var lengths = new Uint32Array(REQUESTS);
var pathLane = 0x31415926;
var queryLane = 0x27182818;
var total = 0;
for (var request = 0; request < REQUESTS; request++) {
  var values = {
    tenant: words[next(words.length)],
    asset: "a-" + next(100000),
    revision: next(9000),
    region: words[next(words.length)],
    report: "r " + next(70000),
    format: (request & 1) ? "json+gzip" : "text/plain",
    category: words[next(words.length)],
    slug: words[next(words.length)] + "-" + next(1000),
    page: 1 + next(80),
    tag: words[next(words.length)],
    team: "team " + next(512),
    build: next(1000000),
    artifact: words[next(words.length)] + ".bin"
  };
  var rendered = expand(templates[request & 3], values);
  lengths[request] = rendered.length;
  total += rendered.length;
  for (var i = request & 7; i < rendered.length; i += 8) {
    var code = rendered.charCodeAt(i);
    pathLane = (pathLane + code + ((queryLane << 5) | (queryLane >>> 27))) | 0;
    pathLane = (pathLane << 9) | (pathLane >>> 23);
    queryLane = (queryLane ^ pathLane ^ (code * 257 + request + i)) | 0;
    queryLane = (queryLane << 15) | (queryLane >>> 17);
  }
}

var lengthFold = 0;
for (var i = 0; i < lengths.length; i += 17) lengthFold = (lengthFold + lengths[i]) >>> 0;
var hash = (pathLane + queryLane + (pathLane >>> 11) + (queryLane << 7)) >>> 0;
var summary = "template-uri=" + REQUESTS + ":" + total + ":" + lengthFold + ":" + hash;
var EXPECTED = "template-uri=36000:1810247:106620:2787392704";
if (summary !== EXPECTED) throw new Error("template URI PGO checksum mismatch: " + summary);
console.log(summary);
