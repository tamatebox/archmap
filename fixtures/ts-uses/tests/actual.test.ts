export async function check(): Promise<string> {
  const { formatPrice } = await vi.importActual<typeof import('../src/money')>('../src/money');
  return formatPrice(4);
}
