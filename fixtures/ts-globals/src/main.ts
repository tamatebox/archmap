import './setup';
import './polyfill.js';
import { mode } from './env';

export function start(): string {
  registry.set(mode, 1);
  window.appReady = true;
  return `${__BUILD__} ${buildMode}`;
}
