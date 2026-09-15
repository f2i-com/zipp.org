"""N-body simulation of the outer planets: float arithmetic over lists.

Adapted from the Computer Language Benchmarks Game program. `**` with a
fractional exponent is replaced by `math.sqrt` so every float operation is
correctly rounded and the printed energies are identical across engines.
"""
import math
import sys
import time

STEPS = 3000
PI = 3.14159265358979323
SOLAR_MASS = 4 * PI * PI
DAYS_PER_YEAR = 365.24


def make_bodies():
    return [
        # sun
        [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0], SOLAR_MASS],
        # jupiter
        [[4.84143144246472090e+00, -1.16032004402742839e+00, -1.03622044471123109e-01],
         [1.66007664274403694e-03 * DAYS_PER_YEAR, 7.69901118419740425e-03 * DAYS_PER_YEAR,
          -6.90460016972063023e-05 * DAYS_PER_YEAR],
         9.54791938424326609e-04 * SOLAR_MASS],
        # saturn
        [[8.34336671824457987e+00, 4.12479856412430479e+00, -4.03523417114321381e-01],
         [-2.76742510726862411e-03 * DAYS_PER_YEAR, 4.99852801234917238e-03 * DAYS_PER_YEAR,
          2.30417297573763929e-05 * DAYS_PER_YEAR],
         2.85885980666130812e-04 * SOLAR_MASS],
        # uranus
        [[1.28943695621391310e+01, -1.51111514016986312e+01, -2.23307578892655734e-01],
         [2.96460137564761618e-03 * DAYS_PER_YEAR, 2.37847173959480950e-03 * DAYS_PER_YEAR,
          -2.96589568540237556e-05 * DAYS_PER_YEAR],
         4.36624404335156298e-05 * SOLAR_MASS],
        # neptune
        [[1.53796971148509165e+01, -2.59193146099879641e+01, 1.79258772950371181e-01],
         [2.68067772490389322e-03 * DAYS_PER_YEAR, 1.62824170038242295e-03 * DAYS_PER_YEAR,
          -9.51592254519715870e-05 * DAYS_PER_YEAR],
         5.15138902046611451e-05 * SOLAR_MASS],
    ]


def pairs_of(bodies):
    out = []
    for i in range(len(bodies)):
        for j in range(i + 1, len(bodies)):
            out.append((bodies[i], bodies[j]))
    return out


def advance(bodies, pairs, dt, steps):
    for _ in range(steps):
        for (r1, v1, m1), (r2, v2, m2) in pairs:
            dx = r1[0] - r2[0]
            dy = r1[1] - r2[1]
            dz = r1[2] - r2[2]
            d2 = dx * dx + dy * dy + dz * dz
            mag = dt / (d2 * math.sqrt(d2))
            b1m = m1 * mag
            b2m = m2 * mag
            v1[0] -= dx * b2m
            v1[1] -= dy * b2m
            v1[2] -= dz * b2m
            v2[0] += dx * b1m
            v2[1] += dy * b1m
            v2[2] += dz * b1m
        for r, v, m in bodies:
            r[0] += dt * v[0]
            r[1] += dt * v[1]
            r[2] += dt * v[2]


def energy(bodies, pairs):
    e = 0.0
    for (r1, v1, m1), (r2, v2, m2) in pairs:
        dx = r1[0] - r2[0]
        dy = r1[1] - r2[1]
        dz = r1[2] - r2[2]
        e -= (m1 * m2) / math.sqrt(dx * dx + dy * dy + dz * dz)
    for r, v, m in bodies:
        e += m * (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]) / 2.0
    return e


def offset_momentum(bodies):
    px = py = pz = 0.0
    for r, v, m in bodies:
        px -= v[0] * m
        py -= v[1] * m
        pz -= v[2] * m
    v = bodies[0][1]
    m = bodies[0][2]
    v[0] = px / m
    v[1] = py / m
    v[2] = pz / m


def bench(steps):
    bodies = make_bodies()
    pairs = pairs_of(bodies)
    offset_momentum(bodies)
    before = energy(bodies, pairs)
    advance(bodies, pairs, 0.01, steps)
    after = energy(bodies, pairs)
    return before, after


t0 = time.perf_counter()
result = bench(STEPS)
elapsed = time.perf_counter() - t0
print("nbody", STEPS, "%.9f" % result[0], "%.9f" % result[1])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
