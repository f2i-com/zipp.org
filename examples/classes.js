// Classes: extends, getters, private fields, static blocks.
class Shape {
  #name;
  static registry = [];
  static { Shape.registry.push("Shape"); }
  constructor(name) { this.#name = name; }
  get name() { return this.#name; }
  area() { return 0; }
  toString() { return `${this.name}(${this.area().toFixed(2)})`; }
}

class Circle extends Shape {
  constructor(r) { super("circle"); this.r = r; }
  area() { return Math.PI * this.r ** 2; }
}

class Rect extends Shape {
  constructor(w, h) { super("rect"); this.w = w; this.h = h; }
  area() { return this.w * this.h; }
}

const shapes = [new Circle(1), new Rect(2, 3), new Circle(0.5)];
for (const s of shapes) console.log(String(s));
console.log("total area:", shapes.reduce((a, s) => a + s.area(), 0).toFixed(3));
console.log("static block ran:", Shape.registry);
