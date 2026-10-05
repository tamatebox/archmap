import { Money, Currency } from './money';
import Price from './money';
import { Line, count } from './lines';

export const total = (items: Money[], currency?: Currency, price?: Price) =>
  count(items as Line[]) + (currency ? 1 : 0) + (price ? 1 : 0);
