export type Money = { amount: number; currency: string };

export interface Priced {
  price: Money;
}

export enum Unit {
  Piece,
  Box,
}

import type { Wallet } from './money';
import type { Request } from '@acme/http';
