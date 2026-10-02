export async function load() {
  const { formatPrice } = await import('../src/money.js');
  return formatPrice(1);
}

export function later() {
  return import('../src/money.js').then((m) => m.formatPrice(2));
}
