// var path = require('path');
// var webpack = require('webpack');




module.exports = function (helper) {
  return {
    devServer: {
      historyApiFallback: true,
      port: 3000,
    },
    performance: {
      hints: false
    },
    devtool: '#eval-source-map'
  }
}
