from bazaar.billing.charge import pay


def by_parameter(pay):
    return pay(1)


def assigned_later():
    print(pay)
    pay = 2
    return pay


def in_comprehension():
    return [pay for pay in range(3)]


class Holder:
    pay = 1
    total = pay

    def method(self):
        return pay(2)


def by_default(handler=pay):
    return handler


pay(3)
