import { it, vi } from 'vitest';
import { play } from '../src/player';

vi.mock('../src/audio', () => ({ unrelated: vi.fn() }));

it('plays', () => play());
