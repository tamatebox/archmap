import { formatPrice, Button, LIMIT, limitOf, money, Missing } from '..';
import * as everything from '..';
import '..';

export function total(n: number): string {
  return [formatPrice({ amount: n, currency: 'JPY' }), LIMIT, limitOf('x'), Button, money, everything, Missing].join(' ');
}
