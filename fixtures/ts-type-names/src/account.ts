import { total } from './basket';

export class Account {
  constructor(public cents: number) {}
}

export const zero = total([]);
