const installDependencies = require('../_installer');

const dependencies = ['file-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.(png|jpg|jpeg|gif|svg)(\?v=\d+\.\d+\.\d+)?$/,
  loader: 'file-loader',
  options: {
    name: '[folder]/[name].[ext]?[hash]',
  },
};
