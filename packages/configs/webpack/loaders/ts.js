const installDependencies = require('../_installer');

const dependencies = ['typescript', 'ts-loader', '@babel/core', 'babel-loader'];

installDependencies(dependencies);

module.exports = {
  test: /\.ts$/,
  use: () => {
    const loaders = [];
    if (process.env.NODE_ENV === 'production') {
      loaders.push('babel-loader');
    }
    loaders.push('ts-loader');
    return loaders;
  },
  exclude: /node_modules/,
};
