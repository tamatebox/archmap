export interface Line {
  cents: number;
}

export function count(items: Line[]): number {
  return items.length;
}
