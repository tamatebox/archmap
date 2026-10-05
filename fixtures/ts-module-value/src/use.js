const slugify = require('./slug');
module.exports = { a: slugify('X'), b: require('./slug')('Y') };
