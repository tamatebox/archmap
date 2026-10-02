const { formatPrice } = require('../src/money.js');
const money = require('../src/money.js');

module.exports = { a: formatPrice(1), b: money.formatPrice(2) };
