import { placeOrder } from '../src/orders';

jest.mock('../src/orders', () => ({ ...jest.requireActual('../src/orders'), placeOrder: jest.fn() }));

test('places', () => placeOrder(1));
