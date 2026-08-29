"use strict";

// Independent parser/string/JSON/RegExp training data. The records, regex, and
// two-lane endpoint checksum use a different shape from publication workloads.
var rows = [];
for (var i = 0; i < 32000; i++) {
  rows.push({
    id: i,
    group: "g" + (i % 37),
    enabled: (i % 5) !== 0,
    label: "item_" + ((i * 2246822507) >>> 0).toString(36),
    values: [i & 255, (i * 7) & 1023, (i * i) & 4095]
  });
}

var encoded = JSON.stringify({ version: 3, rows: rows });
var decoded = JSON.parse(encoded);
var token = /item_([0-9a-z]+)|"group":"(g\d+)"/g;
var matches = 0;
var laneA = 0x6a09e667;
var laneB = 0xbb67ae85;
var match;
while ((match = token.exec(encoded)) !== null) {
  var part = match[1] || match[2];
  matches++;
  var first = part.charCodeAt(0);
  var last = part.charCodeAt(part.length - 1);
  laneA = (laneA + Math.imul(first + part.length, 0x4a39b70d)) | 0;
  laneA = (laneA << 9) | (laneA >>> 23);
  laneB = Math.imul((laneB ^ last) + laneA, 0x27d4eb2d) | 0;
  laneB = (laneB << 13) | (laneB >>> 19);
}
var hash = (laneA ^ laneB ^ (laneA >>> 7) ^ (laneB << 3)) >>> 0;

var normalized = encoded.replace(/item_/g, "entry-");
var pieces = normalized.slice(0, 200000).split("\"");
var check = decoded.rows[12345].values[2] ^ pieces.length ^ matches ^ hash;
var summary = encoded.length + ":" + (check >>> 0);
var EXPECTED = "2791024:2064265531";
if (summary !== EXPECTED) throw new Error("text/data PGO checksum mismatch: " + summary);
console.log(summary);
