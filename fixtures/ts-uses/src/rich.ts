import { Wallet } from './money';

export class Rich extends Wallet {
  spend(): string {
    super.pay(5);
    return this.pay(6);
  }
  static fresh(): Wallet {
    return super.open();
  }
}

export const tip = (w?: Wallet) => w?.pay(7);
export const opened = Rich.open();
