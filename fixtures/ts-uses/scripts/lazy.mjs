export async function load() {
  const { formatPrice } = await import('../src/money.js');
  return formatPrice(1);
}

export function later() {
  return import('../src/money.js').then((m) => m.formatPrice(2));
}

export function picked() {
  return import('../src/money.js').then(({ formatPrice }) => formatPrice(3));
}

export function handed(show) {
  return import('../src/money.js').then(show);
}

export function relayed(send) {
  return import('../src/money.js').then(function (m) {
    const later = () => send(arguments[0]);
    return later();
  });
}
