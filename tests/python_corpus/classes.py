# Classes: inheritance, super, dunders, properties, class/static methods.
class Animal:
    count = 0

    def __init__(self, name, sound="..."):
        self.name = name
        self.sound = sound
        Animal.count += 1

    def speak(self):
        return f"{self.name} says {self.sound}"

    def __repr__(self):
        return f"Animal({self.name!r})"

    def __str__(self):
        return self.name

    def __eq__(self, other):
        return isinstance(other, Animal) and self.name == other.name

    def __hash__(self):
        return hash(self.name)

    def __lt__(self, other):
        return self.name < other.name

    @property
    def loud(self):
        return self.sound.upper() + "!"

    @loud.setter
    def loud(self, value):
        self.sound = value.lower()

    @classmethod
    def make(cls, name):
        return cls(name)

    @staticmethod
    def kingdom():
        return "animalia"


class Dog(Animal):
    def __init__(self, name):
        super().__init__(name, "woof")
        self.tricks = []

    def add_trick(self, trick):
        self.tricks.append(trick)
        return self

    def speak(self):
        base = super().speak()
        return base + " loudly"


class Puppy(Dog):
    def speak(self):
        return "yip: " + super().speak()


a = Animal("cat", "meow")
d = Dog("rex")
p = Puppy("bit")
print(a.speak(), "|", d.speak(), "|", p.speak())
print(repr(a), str(a), a, [a, d])
print(a.loud, end=" ")
a.loud = "PURR"
print(a.sound, a.loud)
print(Animal.count, Dog.count, Dog.kingdom(), Dog.make("made").speak())
print(d.add_trick("sit").add_trick("roll").tricks)
print(a == Animal("cat"), a == d, a != d, hash(a) == hash(Animal("cat")))
print(sorted([d, a, p]), isinstance(p, Animal), isinstance(a, Dog), issubclass(Puppy, Animal))
print(Puppy.__mro__ == (Puppy, Dog, Animal, object), Puppy.__name__, type(p).__name__, type(p) is Puppy)
print(p.__class__.__name__, Dog.__bases__[0].__name__, hasattr(p, "tricks"), hasattr(p, "wings"), getattr(p, "wings", "none"))


class Vector:
    def __init__(self, x, y):
        self.x, self.y = x, y

    def __add__(self, other):
        return Vector(self.x + other.x, self.y + other.y)

    def __sub__(self, other):
        return Vector(self.x - other.x, self.y - other.y)

    def __mul__(self, k):
        return Vector(self.x * k, self.y * k)

    def __rmul__(self, k):
        return self * k

    def __neg__(self):
        return Vector(-self.x, -self.y)

    def __abs__(self):
        return (self.x ** 2 + self.y ** 2) ** 0.5

    def __len__(self):
        return 2

    def __getitem__(self, i):
        return (self.x, self.y)[i]

    def __iter__(self):
        yield self.x
        yield self.y

    def __contains__(self, v):
        return v in (self.x, self.y)

    def __bool__(self):
        return self.x != 0 or self.y != 0

    def __call__(self, scale):
        return Vector(self.x * scale, self.y * scale)

    def __repr__(self):
        return f"Vector({self.x}, {self.y})"


v = Vector(3, 4)
w = Vector(1, 1)
print(v + w, v - w, v * 2, 2 * v, -v, abs(v), len(v), v[0], v[-1], list(v), 4 in v, 5 in v)
print(bool(v), bool(Vector(0, 0)), v(10), tuple(v), [x * 2 for x in v])
x, y = v
print(x, y)


class Counter:
    def __init__(self):
        self.n = 0

    def __enter__(self):
        self.n += 1
        return self

    def __exit__(self, exc_type, exc, tb):
        self.n += 10
        return False


with Counter() as c:
    c.n += 100
print(c.n)


class Meta:
    def __getattr__(self, name):
        return f"dynamic-{name}"

    def __setattr__(self, name, value):
        object.__setattr__(self, name, value * 2)


m = Meta()
m.real = 21
print(m.real, m.anything, "real" in m.__dict__)


class Base:
    def __init_subclass__(cls, **kw):
        cls.registered = True


class Child(Base):
    pass


print(Child.registered, hasattr(Base, "registered"))


class Stack(list):
    def push(self, v):
        self.append(v)
        return self

    def top(self):
        return self[-1]


s = Stack()
s.push(1).push(2)
print(s, len(s), s.top(), isinstance(s, list), s + [3])


class Temp:
    __slots__ = ("c",)

    def __init__(self, c):
        self.c = c

    @property
    def f(self):
        return self.c * 9 / 5 + 32


print(Temp(100).f, Temp(-40).f)
print(str(type(1)), str(type("x")), str(type(None)), type(3.5).__name__, type([]) is list)
