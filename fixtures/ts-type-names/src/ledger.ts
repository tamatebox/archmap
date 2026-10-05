import { Rate, Ledger, Side } from './merged';

export function describe(): string {
  return `${Rate.of(1).value} ${new Ledger()} ${Side.Buy}`;
}
