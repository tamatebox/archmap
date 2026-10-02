import { formatPrice } from '../src/money';

vi.mock('../src/money', async () => ({
  ...(await vi.importActual('../src/money')),
  formatPrice: vi.fn(),
}));

export const partial = formatPrice(3);
