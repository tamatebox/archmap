import { it, vi } from 'vitest';
import { checkout } from '../src/checkout';

vi.mock('../src/orders');

it('checks out', () => checkout());
