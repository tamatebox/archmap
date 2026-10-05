const path = require('path');

module.exports = (env, argv) => ({
  resolve: {
    alias: {
      Utilities: path.resolve(__dirname, 'src/utilities/'),
      Templates$: path.resolve(__dirname, 'src/templates/main.js'),
    },
  },
});
