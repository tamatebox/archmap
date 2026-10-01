import { describe, expect, it, vi } from 'vitest';
import { formatPrice } from '../src/lib/money';
import { makeWallet } from './helpers';

describe('formatPrice', () => {
  it('formats', () => {
    expect(formatPrice({ amount: 1, currency: 'JPY' })).toBe('1 JPY');
    expect(makeWallet()).toBeDefined();
  });
  it('reads limits', async () => {
    expect(await vi.importActual('../src/lib/limits')).toBeDefined();
  });
});

vi.mock('../src/lib/types');
