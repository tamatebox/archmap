export class Wallet {
  static open(): Wallet {
    return new Wallet();
  }
  pay(n: number): number {
    return n;
  }
}
