from bazaar.billing.charge import pay


def reset():
    global pay
    pay = None


def run():
    return pay(1)
