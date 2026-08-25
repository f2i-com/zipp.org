"use strict";

(function main() {
  const epochs = 1800;
  const width = 384;
  const survivors = [];

  function makeObject(value, kind) {
    return { value: value, kind: kind, left: value ^ 85, right: value + 3 };
  }

  function touch(object, delta) {
    object.value = (object.value + delta) | 0;
    return (object.value ^ object.left ^ object.right) | 0;
  }

  let checksum = 0;
  for (let epoch = 0; epoch < epochs; epoch++) {
    const batch = new Array(width);
    for (let i = 0; i < width; i++) {
      batch[i] = makeObject((epoch * width + i) | 0, i & 15);
    }
    for (let i = 0; i < width; i++) {
      checksum = (checksum + touch(batch[i], (epoch + i) & 7)) | 0;
      if (((epoch * width + i) % 97) === 0) survivors.push(batch[i]);
    }
    if (survivors.length > 9000 && (epoch & 31) === 0) {
      survivors.splice(0, 3000);
    }
  }

  for (let i = 0; i < survivors.length; i++) {
    checksum = (checksum ^ survivors[i].value ^ survivors[i].kind) | 0;
  }
  console.log("shapes-stable", checksum, survivors.length, epochs * width);
})();
