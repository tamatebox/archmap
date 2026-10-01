const { pad } = require('./format.cjs');

function plugin(name) {
  return require(`./plugins/${name}`);
}

async function money() {
  return import('../src/lib/money.js');
}

module.exports = { plugin, money, title: pad('report') };
