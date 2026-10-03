from bazaar.billing.charge import Wallet


def run(wallet):
    return Wallet.open(wallet)


class Rich(Wallet):
    pass
