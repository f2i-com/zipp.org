# Scope analysis: a class body binding a name that a nested scope reads from
# the enclosing function, and nested scopes that start at the same offset.


def outer():
    x = "outer"

    class D:
        x = "D"

        def m(self):
            return x

    return D


print(outer()().m(), outer().x)


def class_scope():
    x = "func"

    class K:
        x = "class"
        y = [x for _ in range(1)]

        def m(self):
            return x

        def n(self):
            def deeper():
                return x
            return deeper()

    return K().m(), K.y, K.x, K().n()


print(class_scope())


def make(name):
    class Model:
        name = "default"

        def get(self):
            return name

    return Model().get(), Model.name


print(make("n"))


def counter():
    count = 0

    class C:
        count = 100

        def bump(self):
            nonlocal count
            count += 1
            return count

    c = C()
    return c.bump(), c.bump(), C.count, count


print(counter())


def globals_in_class():
    v = "enclosing"

    class G:
        global v
        v = "global"

        def read(self):
            return v

    return G().read()


print(globals_in_class(), v)


def same_start():
    m = [[1, 2, 3], [4, 5, 6]]
    print(list([row[c] for row in m] for c in range(3)))
    print(list([j for j in range(2)] for i in range(3)))
    print(list({j for j in range(2)} for i in range(2)))
    print(list({j: i for j in range(2)} for i in range(2)))
    fns = list(lambda: i for i in range(3))
    print(len(fns), [f() for f in fns])
    print(sum([j for j in range(i)][0] if i else 0 for i in range(1, 4)))
    print(tuple((lambda q: q + i)(1) for i in range(3)))
    return "done"


print(same_start())
print("end")
