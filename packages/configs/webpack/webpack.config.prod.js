/** @typedef {import('./types').Helper} Helper */

const webpack = require('webpack');

module.exports = function() {
  return {
    plugins: [
      new webpack.DefinePlugin({
        'process.env': {
          NODE_ENV: '"production"',
        },
      }),
      new webpack.LoaderOptionsPlugin({
        minimize: true,
      }),
    ],
  };
};
