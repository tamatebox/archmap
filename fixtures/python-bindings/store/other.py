from store.billing import charge


def run(order):
    return charge.pay(order)
