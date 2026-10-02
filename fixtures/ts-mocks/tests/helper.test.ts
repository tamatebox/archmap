import { it, vi } from 'vitest';
import { checkout } from '../src/checkout';
import { stubOrders } from './stubs';

vi.mock('../src/orders', () => stubOrders());

it('checks out', () => checkout());
