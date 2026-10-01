export type Money = { amount: number; currency: string };

export interface Priced {
  price: Money;
}

export enum Unit {
  Piece,
  Box,
}
