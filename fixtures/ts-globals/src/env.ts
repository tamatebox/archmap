export const mode = 'test';

declare global {
  var mode: string;
  const buildMode: string;
}

export function describeBuild(): string {
  return `${buildMode} ${mode}`;
}

function shadowed(buildMode: string): string {
  return buildMode;
}
