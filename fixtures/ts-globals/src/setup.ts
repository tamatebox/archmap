declare global {
  var registry: Map<string, number>;
}

globalThis.registry = new Map();

export {};
