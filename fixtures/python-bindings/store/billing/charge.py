from store.billing.money import cents


def pay(order):
    return cents(order)


def refund(order):
    return order
