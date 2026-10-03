from typing import TYPE_CHECKING, Literal

if TYPE_CHECKING:
    from bazaar.billing.charge import Wallet


def open_all(wallets: "list[Wallet]") -> "Wallet":
    return wallets[0]


def kind() -> Literal["Wallet"]:
    return "Wallet"
