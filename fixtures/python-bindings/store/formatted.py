from store.billing import charge


def run(order):
    print(f"{charge.pay(order)}")
    return charge.refund(order)
