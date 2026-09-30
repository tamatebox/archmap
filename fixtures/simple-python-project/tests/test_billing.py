from shop.billing import pay
from shop.users import User


def test_pay():
    assert pay(User(1), 10).amount == 10
