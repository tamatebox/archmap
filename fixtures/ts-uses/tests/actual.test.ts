export async function check(): Promise<string> {
  const { formatPrice } = await vi.importActual<typeof import('../src/money')>('../src/money');
  return formatPrice(4);
}

export function later(): Promise<string> {
  return vi.importActual<typeof import('../src/money')>('../src/money').then(function (m) {
    return m.formatPrice(5);
  });
}
