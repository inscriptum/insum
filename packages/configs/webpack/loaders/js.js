const installDependencies = require('../_installer');

const dependencies = ['@babel/core', 'babel-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.js$/,
  loader: 'babel-loader',
  exclude: '/node_modules/',
};
