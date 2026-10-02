import { it, vi } from 'vitest';
import { send } from '../src/upload';

vi.mock('../src/storage', () => ({ upload: vi.fn() }));

it('sends', () => send());
