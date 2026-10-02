import { it, vi } from 'vitest';
import { placeOrder } from '../src/orders';

vi.mock('../src/orders', () => ({ placeOrder: vi.fn() }));

it('places', () => placeOrder(1));
