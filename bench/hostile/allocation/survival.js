"use strict";

(function main() {
  const epochs = 90;
  const width = 5000;
  let survivors = [];
  let checksum = 0;

  function makeNode(serial) {
    const bias = serial & 255;
    return {
      serial,
      label: "node-" + (serial & 1023),
      values: [bias, bias ^ 85, (bias + 17) & 255],
      apply(delta) {
        return (this.serial + this.values[delta % 3] + delta) | 0;
      }
    };
  }

  for (let epoch = 0; epoch < epochs; epoch++) {
    const fresh = new Array(width);
    for (let i = 0; i < width; i++) {
      const serial = epoch * width + i;
      const node = makeNode(serial);
      fresh[i] = node;
      checksum = (checksum ^ node.apply(epoch + i)) | 0;
      if ((serial % 11) === 0) survivors.push(node);
    }

    for (let i = epoch & 7; i < survivors.length; i += 29) {
      checksum = (checksum + survivors[i].apply(epoch)) | 0;
    }

    // Retain objects across several collections, then discard an old half.
    if (survivors.length > 24000) {
      survivors = survivors.slice(survivors.length >> 1);
    }
  }

  for (let i = 0; i < survivors.length; i += 7) {
    checksum = (checksum ^ survivors[i].serial ^ survivors[i].label.length) | 0;
  }
  console.log("allocation-survival", checksum, survivors.length, epochs * width);
})();
