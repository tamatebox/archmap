import path from 'node:path';

export default {
  resolve: {
    alias: {
      lodash: path.resolve(__dirname, 'src/shims/lodash'),
    },
  },
};
