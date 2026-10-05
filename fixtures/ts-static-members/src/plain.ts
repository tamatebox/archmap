import { Wallet } from './wallet';

export const fresh = new Wallet();
export const isWallet = (x: unknown): x is Wallet => x instanceof Wallet;
