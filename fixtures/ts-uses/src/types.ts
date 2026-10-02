import type { Wallet } from './money';

export type Holder = { wallet: Wallet };
export type Formatter = typeof import('./money').formatPrice;
