"""Charges and the wallet they draw on."""


def pay(amount):
    return amount


def refund(amount):
    return pay(-amount)


class Wallet:
    def open(self):
        return self

    def topup(self, amount):
        self.open()
        return pay(amount)

    @classmethod
    def make(cls):
        return cls.open(cls())

    @staticmethod
    def check(value):
        return value.open()


RATE = 3
