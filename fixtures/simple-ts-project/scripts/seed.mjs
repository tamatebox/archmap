import { readFile } from 'node:fs/promises';
import { MAX_UPLOAD } from '../src/lib/limits.js';

export async function seed() {
  return [await readFile('seed.json'), MAX_UPLOAD];
}
