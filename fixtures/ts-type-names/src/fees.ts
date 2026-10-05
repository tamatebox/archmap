import { Fee, Flag } from './kinds';

export function charge(fee: Fee, flag: Flag): number {
  return flag === Flag.On ? fee.cents : 0;
}
