import { Account } from './account';

function Inject(): ParameterDecorator {
  return () => {};
}

export class Service {
  constructor(@Inject() private account: Account) {}
}
