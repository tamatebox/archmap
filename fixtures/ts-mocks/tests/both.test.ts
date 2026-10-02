import { it, vi } from 'vitest';
import { placeOrder } from '../src/orders';

vi.mock('../src/orders', () => ({ placeOrder: vi.fn() }));

it('compares', async () => {
  const real = await vi.importActual<{ placeOrder: typeof placeOrder }>('../src/orders');
  return [placeOrder(1), real.placeOrder(1)];
});
