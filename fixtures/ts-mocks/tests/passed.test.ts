import { it, vi } from 'vitest';
import { checkout } from '../src/checkout';

function partial(load: () => Promise<object>): Promise<object> {
  return load();
}

vi.mock('../src/orders', (importOriginal) => partial(importOriginal));

it('checks out', () => checkout());
