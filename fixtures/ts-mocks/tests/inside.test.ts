import { describe, it, vi } from 'vitest';
import { checkout } from '../src/checkout';

describe('checkout', () => {
  vi.mock('../src/orders', () => ({ placeOrder: vi.fn() }));
  it('checks out', () => checkout());
});
