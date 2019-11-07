const installDependencies = require('../_installer');

const dependencies = ['file-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.(eot|woff|woff2|ttf)(\?v=\d+\.\d+\.\d+)?$/,
  use: 'file-loader?name=[folder]/[name].[ext]',
};
