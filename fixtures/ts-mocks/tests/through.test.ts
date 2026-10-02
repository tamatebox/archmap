import { it, vi } from 'vitest';
import { checkout } from '../src/checkout';

vi.mock('../src/orders', () => ({ placeOrder: vi.fn().mockReturnValue(2) }));

it('checks out', () => checkout());
