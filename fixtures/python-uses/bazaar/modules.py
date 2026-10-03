import bazaar.billing.charge
from bazaar.billing import charge


def run():
    bazaar.billing.charge.pay(1)
    return charge.pay(2)
