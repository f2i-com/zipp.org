// TypeScript bench: a small ray tracer. Classes with readonly parameter
// properties, interfaces, a generic scene, getters and optional chaining.
// Prints a checksum of the rendered image.

interface Ray {
  readonly origin: Vec;
  readonly dir: Vec;
}

class Vec {
  constructor(
    readonly x: number,
    readonly y: number,
    readonly z: number,
  ) {}
  add(o: Vec): Vec { return new Vec(this.x + o.x, this.y + o.y, this.z + o.z); }
  sub(o: Vec): Vec { return new Vec(this.x - o.x, this.y - o.y, this.z - o.z); }
  scale(k: number): Vec { return new Vec(this.x * k, this.y * k, this.z * k); }
  dot(o: Vec): number { return this.x * o.x + this.y * o.y + this.z * o.z; }
  get length(): number { return Math.sqrt(this.dot(this)); }
  norm(): Vec { return this.scale(1 / this.length); }
}

interface Material {
  color: Vec;
  reflect: number;
}

interface Hit {
  t: number;
  point: Vec;
  normal: Vec;
  material: Material;
}

abstract class Shape {
  constructor(public readonly material: Material) {}
  abstract intersect(ray: Ray): number | undefined;
  abstract normalAt(p: Vec): Vec;
}

class Sphere extends Shape {
  constructor(private readonly center: Vec, private readonly radius: number, material: Material) {
    super(material);
  }
  intersect(ray: Ray): number | undefined {
    const oc = ray.origin.sub(this.center);
    const b = oc.dot(ray.dir);
    const c = oc.dot(oc) - this.radius * this.radius;
    const disc = b * b - c;
    if (disc < 0) return undefined;
    const t = -b - Math.sqrt(disc);
    return t > 1e-4 ? t : undefined;
  }
  normalAt(p: Vec): Vec { return p.sub(this.center).norm(); }
}

class Plane extends Shape {
  constructor(private readonly normal: Vec, private readonly offset: number, material: Material) {
    super(material);
  }
  intersect(ray: Ray): number | undefined {
    const denom = this.normal.dot(ray.dir);
    if (Math.abs(denom) < 1e-6) return undefined;
    const t = -(this.normal.dot(ray.origin) + this.offset) / denom;
    return t > 1e-4 ? t : undefined;
  }
  normalAt(_p: Vec): Vec { return this.normal; }
}

class Scene<S extends Shape = Shape> {
  private readonly shapes: S[] = [];
  constructor(readonly light: Vec) {}
  add(shape: S): this { this.shapes.push(shape); return this; }
  nearest(ray: Ray): Hit | null {
    let best: Hit | null = null;
    for (const s of this.shapes) {
      const t = s.intersect(ray);
      if (t !== undefined && (best === null || t < best.t)) {
        const point = ray.origin.add(ray.dir.scale(t));
        best = { t, point, normal: s.normalAt(point), material: s.material };
      }
    }
    return best;
  }
}

function trace(scene: Scene, ray: Ray, depth: number): Vec {
  const hit = scene.nearest(ray);
  if (!hit) return new Vec(0.1, 0.1, 0.15);
  const toLight = scene.light.sub(hit.point).norm();
  const shadow = scene.nearest({ origin: hit.point, dir: toLight });
  const diffuse = shadow && shadow.t < scene.light.sub(hit.point).length ? 0.1 : Math.max(0, hit.normal.dot(toLight));
  let color = hit.material.color.scale(0.1 + 0.9 * diffuse);
  if (depth > 0 && hit.material.reflect > 0) {
    const dir = ray.dir.sub(hit.normal.scale(2 * ray.dir.dot(hit.normal)));
    color = color.scale(1 - hit.material.reflect).add(trace(scene, { origin: hit.point, dir }, depth - 1).scale(hit.material.reflect));
  }
  return color;
}

const scene = new Scene(new Vec(5, 8, -3))
  .add(new Sphere(new Vec(0, 1, 3), 1, { color: new Vec(0.9, 0.2, 0.2), reflect: 0.3 }))
  .add(new Sphere(new Vec(-2, 0.7, 4), 0.7, { color: new Vec(0.2, 0.9, 0.3), reflect: 0.5 }))
  .add(new Sphere(new Vec(2, 0.5, 2.5), 0.5, { color: new Vec(0.2, 0.3, 0.9), reflect: 0.1 }))
  .add(new Plane(new Vec(0, 1, 0), 0, { color: new Vec(0.8, 0.8, 0.8), reflect: 0.2 }));

const W = 320, H = 240;
let checksum = 0;
const eye = new Vec(0, 1.5, -4);
for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    const dir = new Vec((x - W / 2) / H, (H / 2 - y) / H, 1).norm();
    const c = trace(scene, { origin: eye, dir }, 3);
    checksum = (checksum + ((c.x * 255) | 0) * 3 + ((c.y * 255) | 0) * 5 + ((c.z * 255) | 0) * 7) % 1000000007;
  }
}
console.log(checksum);
