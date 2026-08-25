"use strict";

(function main() {
  const epochs = 1800;
  const width = 384;
  const survivors = [];

  function makeObject(value, kind) {
    switch (kind & 15) {
      case 0: return { value, kind, left: value ^ 85, right: value + 3, a0: 0 };
      case 1: return { a1: 1, value, kind, left: value ^ 85, right: value + 3 };
      case 2: return { kind, a2: 2, value, right: value + 3, left: value ^ 85 };
      case 3: return { left: value ^ 85, kind, a3: 3, right: value + 3, value };
      case 4: return { right: value + 3, value, a4: 4, left: value ^ 85, kind };
      case 5: return { a5: 5, left: value ^ 85, value, kind, right: value + 3 };
      case 6: return { kind, right: value + 3, a6: 6, value, left: value ^ 85 };
      case 7: return { left: value ^ 85, a7: 7, kind, value, right: value + 3 };
      case 8: return { a8: 8, right: value + 3, left: value ^ 85, value, kind };
      case 9: return { value, a9: 9, right: value + 3, kind, left: value ^ 85 };
      case 10: return { kind, left: value ^ 85, right: value + 3, a10: 10, value };
      case 11: return { right: value + 3, kind, value, left: value ^ 85, a11: 11 };
      case 12: return { a12: 12, value, left: value ^ 85, kind, right: value + 3 };
      case 13: return { left: value ^ 85, right: value + 3, value, a13: 13, kind };
      case 14: return { kind, a14: 14, value, left: value ^ 85, right: value + 3 };
      default: return { right: value + 3, a15: 15, kind, left: value ^ 85, value };
    }
  }

  // This shared site sees sixteen layouts, and some objects deliberately enter
  // dictionary mode after warming the ordinary shapes.
  function touch(object, delta, serial) {
    if ((serial & 4095) === 17) {
      delete object.left;
      object.left = object.value ^ 85;
    }
    object.value = (object.value + delta) | 0;
    return (object.value ^ object.left ^ object.right) | 0;
  }

  let checksum = 0;
  for (let epoch = 0; epoch < epochs; epoch++) {
    const batch = new Array(width);
    for (let i = 0; i < width; i++) {
      const serial = (epoch * width + i) | 0;
      batch[i] = makeObject(serial, (i + epoch) & 15);
    }
    for (let i = 0; i < width; i++) {
      const serial = (epoch * width + i) | 0;
      checksum = (checksum + touch(batch[i], (epoch + i) & 7, serial)) | 0;
      if ((serial % 97) === 0) survivors.push(batch[i]);
    }
    if (survivors.length > 9000 && (epoch & 31) === 0) {
      survivors.splice(0, 3000);
    }
  }

  for (let i = 0; i < survivors.length; i++) {
    checksum = (checksum ^ survivors[i].value ^ survivors[i].kind) | 0;
  }
  console.log("shapes-megamorphic", checksum, survivors.length, epochs * width);
})();
