"use strict";

// Independent insertion/deletion/reinsertion workload for the dictionary-mode
// property paths.  The scored suites use different key names, loop shapes,
// widths, retention, and checksums; this file supplies representative profile
// coverage without training on either publication program.
function mutateRecords(seed) {
  var checksum = 0;
  var retained = new Array(257);

  for (var row = 0; row < 18000; row++) {
    var record = {};
    var bias = (seed + row) & 15;

    for (var field = 0; field < 48; field++) {
      record["field$" + field] = (field * 3 + bias) | 0;
    }
    for (var field = 2; field < 48; field += 3) {
      delete record["field$" + field];
    }
    for (var field = 47; field >= 2; field -= 3) {
      record["field$" + field] = (field * 5 + bias) | 0;
    }
    for (var field = 0; field < 48; field++) {
      checksum = (checksum + record["field$" + field]) | 0;
    }

    var count = 0;
    for (var key in record) count++;
    checksum = (checksum ^ count) | 0;
    retained[row % retained.length] = record;
  }

  for (var i = 0; i < retained.length; i++) {
    checksum = (checksum + retained[i]["field$" + (i % 48)]) | 0;
  }
  return checksum;
}

var result = mutateRecords(23);
if (result !== 80951254) {
  throw new Error("dictionary PGO checksum mismatch: " + result);
}
console.log(result);
