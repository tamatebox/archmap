from store.billing import charge


def run(order: "charge.Receipt"):
    return charge.refund(order)
