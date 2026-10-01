import { z } from 'zod';
import { readFileSync } from 'node:fs';
import path from 'path';
import type { Money } from './types';

export const CURRENCY = 'JPY';

export function formatPrice(price: Money): string {
  return `${price.amount} ${CURRENCY}`;
}

export class Wallet {
  pay(amount: number): void {}
  private audit(): void {}
  static open(): Wallet {
    return new Wallet();
  }
}

export const schema = z.object({ amount: z.number() });
const rates = { JPY: 1 };
export { rates as RATES };
export const read = () => readFileSync(path.join('a', 'b'));
