import { formatPrice, Button, LIMIT, limitOf, money, Missing } from '..';
import shop, * as everything from '..';
import '..';

export function total(n: number): string {
  return [formatPrice({ amount: n, currency: 'JPY' }), LIMIT, limitOf('x'), shop('y'), Button, money, everything, Missing].join(' ');
}
import type { Money } from '..';
import { Money as Price } from '..';
