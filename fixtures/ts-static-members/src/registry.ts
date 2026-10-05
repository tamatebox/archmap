import { Wallet } from './wallet';

export function make(kind: { open(): unknown }) {
  return kind.open();
}

export const made = make(Wallet);
