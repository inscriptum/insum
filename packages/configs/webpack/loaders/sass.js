const installDependencies = require('../_installer');

const dependencies = [['sass', 'node-sass'], 'raw-loader', 'postcss-loader', 'postcss-css-variables', 'sass-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.(sa|sc|c)ss$/,
  use: [
    {
      loader: 'raw-loader',
    },
    {
      loader: 'postcss-loader',
    },
    {
      loader: 'sass-loader',
    },
  ],
};
