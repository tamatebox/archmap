export function formatPrice(amount: number): string {
  return `${amount} JPY`;
}

export class Wallet {
  static open(): Wallet {
    return new Wallet();
  }
  pay(amount: number): string {
    return formatPrice(amount);
  }
  settle(): void {
    this.pay(1);
    const later = function (this: Wallet) {
      this.pay(2);
    };
    [3].forEach((n) => this.pay(n));
    later.call(this);
  }
  static reopen(): Wallet {
    return this.open();
  }
}

const rates = { JPY: 1 };
export { rates as RATES };

function total(values: number[]): number {
  return values.reduce((a, b) => a + b, 0) * rates.JPY;
}
export default total;
export { formatPrice as fp3 };
