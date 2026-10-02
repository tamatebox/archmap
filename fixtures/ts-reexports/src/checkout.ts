import { formatPrice } from './money';

export { formatPrice };

export function total(n: number): string {
  return formatPrice(n);
}
