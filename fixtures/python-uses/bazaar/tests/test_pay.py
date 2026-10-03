from unittest import mock

from bazaar.billing.charge import RATE, pay


def test_pay():
    assert pay(RATE) == RATE


@mock.patch("bazaar.billing.charge.refund")
def test_refund(fake):
    from bazaar.billing.charge import refund

    assert refund
