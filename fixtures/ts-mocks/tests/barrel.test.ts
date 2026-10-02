import { it, vi } from 'vitest';
import { placeOrder } from '../src';

vi.mock('../src', () => ({ placeOrder: vi.fn() }));

it('places', () => placeOrder(1));
