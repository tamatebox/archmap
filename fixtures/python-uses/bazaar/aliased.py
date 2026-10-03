from bazaar.billing.charge import pay as settle


def run(order):
    return settle(order)
