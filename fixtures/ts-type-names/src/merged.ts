import { describe } from './ledger';

export interface Rate {
  value: number;
}

export const Rate = { of: (value: number): Rate => ({ value }) };

export class Ledger {}

export enum Side {
  Buy,
  Sell,
}

export const label = describe();
