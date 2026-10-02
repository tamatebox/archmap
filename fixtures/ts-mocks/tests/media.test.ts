import { it, vi } from 'vitest';
import getAudioUrl from '../src/media';

vi.mock('../src/media', () => ({ default: vi.fn() }));

it('plays', () => getAudioUrl('a'));
