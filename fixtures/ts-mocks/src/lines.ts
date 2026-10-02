import type { Price } from './types';
import { price } from './pricing';

export type Line = { price: Price };

export const total = (n: number): number => price(n);
