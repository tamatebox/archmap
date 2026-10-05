import { Account } from '../src/account';
import { type Account as Marked } from '../src/account';

export const kept = (a: Account, b: Marked) => a.cents + b.cents;
