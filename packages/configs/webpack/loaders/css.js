const installDependencies = require('../_installer');

const dependencies = ['raw-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.css$/,
  use: [
    {
      loader: 'raw-loader',
    },
  ],
};
