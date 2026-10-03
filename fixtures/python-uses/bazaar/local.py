def run():
    from bazaar.billing.charge import pay

    return [pay(n) for n in range(3)]


def lazy(amount):
    total = (lambda: pay)(amount)
    return total
