print(-5 // 2, -5 % 2, 5 // -2, 5 % -2)
print(9007199254740993 + 10)
print(bool([]), bool(()), bool(range(0)), bool([0]))
values = [1, 2]
alias = values
values += [3]
values[-1] = 4
print(values, alias)
print(len("A😀B"), "A😀B"[-2])
print([1] == (1,), (1,))
for i in range(5):
    if i == 2:
        continue
    print(i)
else:
    print("complete")
