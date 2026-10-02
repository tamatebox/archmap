from store.billing import charge


def run(registry):
    registry.add(charge)
    return charge.refund
