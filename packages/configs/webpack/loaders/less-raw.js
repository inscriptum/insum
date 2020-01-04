const installDependencies = require('../_installer');

const dependencies = ['raw-loader', 'postcss-loader', 'less-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.less$/,
  use: [
    {
      loader: 'raw-loader',
    },
    {
      loader: 'postcss-loader',
    },
    {
      loader: 'less-loader',
    },
  ],
};
