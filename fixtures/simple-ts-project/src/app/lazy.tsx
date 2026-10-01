import { lazy } from 'react';

export const Chart = lazy(() => import('../components/button').then((m) => ({ default: m.Button })));
export type Limits = typeof import('../lib/limits');
export type Purse = import('../lib/money').Wallet;
export function load(name: string) {
  return import(`./${name}`);
}
