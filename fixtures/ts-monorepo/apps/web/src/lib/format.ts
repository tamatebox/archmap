import en from '@acme/i18n/en.json';

export function format(n: number): string {
  return `${en.hello} ${n}`;
}
