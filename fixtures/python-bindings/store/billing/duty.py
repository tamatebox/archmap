from store.billing.levy import rate_of
from store.billing.money import cents


def charge_duty(order):
    return cents(rate_of(order))
