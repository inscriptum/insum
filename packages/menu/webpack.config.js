const path = require('path')
const merge = require('webpack-merge');
const CleanPlugin = require('clean-webpack-plugin');
const HtmlWebpackPlugin = require('html-webpack-plugin');

const webpackConfigCommon = require('@insum/config/webpack/webpack.config.common.js');
const webpackConfigDev = require('@insum/config/webpack/webpack.config.dev.js');
const webpackConfigProd = require('@insum/config/webpack/webpack.config.prod.js');


let helper = {
  PATHS: {
    root: path.join(__dirname, './'),
    src: path.join(__dirname, './src'),
    dev: path.join(__dirname, './dev'),
    node_modules: path.join(__dirname, './node_modules'),
    dist: path.join(__dirname, './dist'),
    publicPath: '',
    outputPath: '' //process.env.NODE_ENV === 'production' ? 'public/' : ''
  },
  TARGET: process.env.npm_lifecycle_event
}


let PREBUILD_CFG = {
  entry: {
    'index': path.resolve(helper.PATHS.src, 'main.ts'),
  },
  plugins: [
    new CleanPlugin([helper.PATHS.dist], {
      root: helper.PATHS.root,
      dry: false,
      verbose: true,
    }),
  ]
}


if (process.env.NODE_ENV === 'production') {
  module.exports = [merge(PREBUILD_CFG, webpackConfigCommon(helper), webpackConfigProd(helper))];
} else {

  module.exports = [
    merge(
      PREBUILD_CFG,
      webpackConfigCommon(helper),
      webpackConfigDev(helper), {
        plugins: [
          new HtmlWebpackPlugin({
            filename: 'index.html',
            template: path.resolve(helper.PATHS.root, 'index.html'),
          }),
        ]
      }
    )
  ];
}