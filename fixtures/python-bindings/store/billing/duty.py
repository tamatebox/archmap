from store.billing.levy import rate_of


def charge_duty(order):
    return rate_of(order)
