const MAX_UPLOAD = 10;

function limitOf(name: string): number {
  return name.length > 0 ? MAX_UPLOAD : 0;
}

export { MAX_UPLOAD };
export default limitOf;
