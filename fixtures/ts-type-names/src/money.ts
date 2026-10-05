import { total } from './cart';

export interface Money {
  cents: number;
}

export type Currency = 'JPY' | 'USD';

export default interface Price {
  money: Money;
}

export const empty = total([]);
