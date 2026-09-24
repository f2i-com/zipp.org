# Raising and catching through the native paths: exception instances and
# classes, user subclasses (with and without __init__), context chaining,
# re-raise, raise from, exceptions from runtime helpers, handler order,
# finally, nested handlers, and class attributes read through instances.
import sys


class AppError(Exception):
    pass


class CodeError(AppError):
    def __init__(self, code):
        super().__init__("code %d" % code)
        self.code = code


class Meta(type):
    pass


def classify(i):
    try:
        if i % 5 == 0:
            raise AppError("five", i)
        if i % 5 == 1:
            raise CodeError(i)
        if i % 5 == 2:
            raise KeyError
        if i % 5 == 3:
            {}["missing%d" % i]
        return "none"
    except CodeError as e:
        return "code:%s:%d:%r" % (e, e.code, e.args)
    except AppError as e:
        return "app:%r:%s" % (e.args, e)
    except KeyError as e:
        return "key:%r" % (e.args,)


print([classify(i) for i in range(10)])
e = AppError("x")
for i in range(3):
    try:
        raise e
    except AppError as caught:
        print(caught is e, caught.args, caught.__context__, caught.__cause__)
try:
    try:
        raise ValueError("inner")
    except ValueError:
        raise AppError("outer")
except AppError as err:
    print(repr(err), repr(err.__context__), err.__suppress_context__)
try:
    try:
        raise ValueError("inner")
    except ValueError as v:
        raise AppError("outer") from v
except AppError as err:
    print(repr(err.__cause__), repr(err.__context__), err.__suppress_context__)
try:
    try:
        raise ValueError("inner")
    except ValueError:
        raise AppError("outer") from None
except AppError as err:
    print(repr(err.__cause__), repr(err.__context__), err.__suppress_context__)
try:
    try:
        raise ValueError("again")
    except ValueError:
        raise
except ValueError as err:
    print("reraised", err)
try:
    raise AppError
except AppError as err:
    print("class raised", repr(err), err.args)
try:
    raise 5
except TypeError as err:
    print("TypeError:", err)
try:
    raise Meta
except TypeError as err:
    print("TypeError:", err)
same = AppError("same")
try:
    try:
        raise same
    except AppError:
        raise same
except AppError as err:
    print("self-context", err.__context__ is None)
log = []
for i in range(4):
    try:
        try:
            if i % 2:
                raise CodeError(i)
            log.append("ok%d" % i)
        finally:
            log.append("fin%d" % i)
    except CodeError as err:
        log.append("caught%d" % err.code)
print(log)
print(sys.exc_info()[0])


def gen_raise():
    try:
        yield 1
        raise AppError("from gen")
    except AppError as err:
        yield "gen caught %s" % err


print(list(gen_raise()))
print(isinstance(CodeError(3), AppError), issubclass(CodeError, Exception), CodeError(4).args, str(CodeError(5)), repr(KeyError("k")), str(KeyError("k")), KeyError().args)
print(AppError(1, 2).args, str(AppError(1, 2)), str(AppError()), repr(AppError("q")), ValueError("v") == ValueError("v"))


class Account:
    fee = 0
    rate = 1.5
    name = "acct"
    flag = True

    def __init__(self):
        self.cents = 100

    def charge(self):
        self.cents -= self.fee
        return self.cents, self.rate, self.name, self.flag


class Checking(Account):
    fee = 25


a, c = Account(), Checking()
print([a.charge() for _ in range(2)], [c.charge() for _ in range(2)])
Account.fee = 7
print(a.charge(), c.charge())
c.fee = 1
print(c.charge(), Checking.fee)
del c.fee
print(c.charge())
del Checking.fee
print(c.charge())
Account.fee = property(lambda self: 1000)
print(a.charge(), c.charge())
Account.name = None
print(a.charge())
