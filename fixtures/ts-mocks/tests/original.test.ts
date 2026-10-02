import { it, vi } from 'vitest';
import { checkout } from '../src/checkout';

vi.mock('../src/orders', async (importOriginal) => ({ ...(await importOriginal<object>()), extra: 1 }));

it('checks out', () => checkout());
