import os
from dataclasses import dataclass


@dataclass
class User:
    id: int

    def display(self) -> str:
        return f"user-{self.id}"

    def _internal(self):
        pass


def _load_env():
    return os.environ
