try:
    from bazaar.billing.charge import pay
except ImportError:
    pay = None


def run():
    return pay(1)
