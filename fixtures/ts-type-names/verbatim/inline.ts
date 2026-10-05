import { type Account } from '../src/account';
import type { Account as Whole } from '../src/account';

export const inline = (a: Account, b: Whole) => a.cents + b.cents;
export { type Account as Passed } from '../src/account';
