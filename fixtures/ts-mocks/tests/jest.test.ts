import { placeOrder } from '../src/orders';

jest.mock('../src/orders', () => ({ placeOrder: jest.fn() }));

test('places', () => placeOrder(1));
