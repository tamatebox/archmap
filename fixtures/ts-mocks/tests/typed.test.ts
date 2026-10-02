import { it, vi } from 'vitest';
import type { Line } from '../src/lines';
import { cart } from '../src/cart';

vi.mock('../src/lines', () => ({ total: vi.fn(() => 5) }));

const sample: Line = { price: { amount: 1 } };

it('totals', () => [cart(), sample]);
