import { formatPrice } from '../src/money';

vi.mock('../src/money', () => ({
  formatPrice: vi.fn(),
  Wallet: vi.fn(),
}));

export const checked = formatPrice(2);
