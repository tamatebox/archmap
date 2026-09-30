import requests
import yaml
from sqlalchemy import select

from ..users import User
from shop import VERSION

CURRENCY = "JPY"


class Payment:
    """A payment.

    def not_real():
    """

    def __init__(self, user: User, amount: int):
        self.user = user
        self.amount = amount

    def charge(self) -> bool:
        return requests.post("http://example", json={"v": VERSION}).ok


def pay(
    user: User,
    amount: int,
) -> Payment:
    return Payment(user, amount)


def _helper():
    pass
