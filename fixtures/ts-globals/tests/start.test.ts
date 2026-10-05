import '../src/setup';
import { start } from '../src/main';

declare global {
  var testOnly: string;
}

start();
