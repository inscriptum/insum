const installDependencies = require('../_installer');

const dependencies = ['html-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.html$/,
  use: ['html-loader'],
};
