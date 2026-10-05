import { Money, Line, Rate } from './barrel';

export function price(money: Money, line: Line, rate: Rate): number {
  return money.cents + line.cents + rate.value + Rate.of(0).value;
}
