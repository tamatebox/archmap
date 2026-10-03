from bazaar.billing import charge


def run(handler):
    return handler(charge)
