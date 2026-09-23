# random matches CPython's Mersenne Twister stream: seeding, the derived
# integer and sequence functions, the distributions, Random instances and
# their state.
import copy
import random

for seed in (0, 1, 42, 2**40 + 7, -5, 12345678901234567890, True):
    random.seed(seed)
    print(seed, random.random(), random.random(), random.getrandbits(8))
random.seed(0)
print([random.randint(1, 6) for _ in range(12)])
print([random.randrange(10) for _ in range(8)], random.randrange(5, 50, 5), random.randrange(10, 0, -3), random.randrange(2**70))
print(random.choice("abcdefg"), random.choice([10, 20, 30]), random.choice(range(1000)))
xs = list(range(20))
random.shuffle(xs)
print(xs)
print(random.sample(range(100), 5), random.sample(range(10000), 12), random.sample("abcdefgh", 8))
print(random.sample(["r", "g", "b"], counts=[3, 2, 1], k=4))
print(random.choices("abc", k=6), random.choices(range(5), weights=[1, 0, 3, 0, 6], k=6))
print(random.choices(["x", "y"], cum_weights=[0.25, 1.0], k=5))
print(random.uniform(-1, 1), random.triangular(0, 10, 3), random.triangular())
print(random.gauss(0, 1), random.gauss(0, 1), random.gauss(5, 2))
print(random.normalvariate(0, 1), random.lognormvariate(0, 0.5), random.expovariate(2.0))
print(random.gammavariate(2.5, 1.0), random.gammavariate(0.5, 2.0), random.gammavariate(1.0, 3.0), random.betavariate(2, 3))
print(random.paretovariate(3.0), random.weibullvariate(1.0, 1.5))
print(random.getrandbits(1), random.getrandbits(32), random.getrandbits(33), random.getrandbits(100), random.randbytes(5))
for s in ("hello", b"bytes", 3.5, -2.25, 0.0):
    random.seed(s)
    print(repr(s), random.random())
random.seed("hello", version=1)
print("v1", random.random())

a, b = random.Random(7), random.Random(7)
print(a.random() == b.random(), a.randint(0, 100), b.randint(0, 100), random.Random().random() < 1.0)
state = a.getstate()
first = [a.random() for _ in range(3)]
a.setstate(state)
print(first == [a.random() for _ in range(3)], state[0], len(state[1]), state[2])
g = random.Random(3)
g.gauss(0, 1)
st = g.getstate()
print(st[2] is not None, g.gauss(0, 1) == (g.setstate(st) or g.gauss(0, 1)))
c = copy.deepcopy(g)
print(c.random() == g.random(), type(c).__name__, isinstance(c, random.Random))


class MyRandom(random.Random):
    def __init__(self):
        super().__init__(99)
        self.draws = 0

    def roll(self):
        self.draws += 1
        return self.randint(1, 20)


m = MyRandom()
print([m.roll() for _ in range(5)], m.draws, MyRandom().random() == random.Random(99).random())
random.seed(2024)
state = random.getstate()
v1 = random.random()
random.setstate(state)
print(v1 == random.random())
for bad in (lambda: random.randrange(0), lambda: random.randrange(5, 5), lambda: random.choice([]), lambda: random.sample([1, 2], 3), lambda: random.randrange(1, 10, 0)):
    try:
        bad()
    except (ValueError, IndexError) as e:
        print(type(e).__name__)
