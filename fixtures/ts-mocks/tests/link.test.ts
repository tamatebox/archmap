import { it, vi } from 'vitest';
import { fileUrl } from '../src/storage';

vi.mock('../src/storage', () => ({ fileUrl: vi.fn() }));

it('links', () => fileUrl('a'));
