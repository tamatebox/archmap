import { formatPrice as fp, Wallet } from './money';
import * as m from './money';
import total from './money';

const { formatPrice: g } = m;

export function run(send: (value: unknown) => void): string {
  send(m);
  const wallet: Wallet = Wallet.open();
  new Wallet().pay(1);
  return [fp(1), m.formatPrice(2), m['formatPrice'](3), g(4), String(total([1])), wallet].join(' ');
}

export function shadowed(fp: (n: number) => string): string {
  return fp(5);
}
