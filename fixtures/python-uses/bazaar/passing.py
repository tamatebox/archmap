from bazaar.billing import pay, settle


def run():
    return pay(1), settle(2)
