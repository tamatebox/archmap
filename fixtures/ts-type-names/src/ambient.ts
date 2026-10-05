import { Account } from './account';
import * as books from './account';

declare class Wrapped extends Account {}
import Alias = books.Account;

export type Kept = Wrapped;
export const alias = Alias;
