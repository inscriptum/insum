module.exports = function (helper) {
  return {
    mode: process.env.NODE_ENV,
    output: {
      path: helper.PATHS.dist,
      publicPath: '/',
      filename: '[name].js',
    },
    module: {
      rules: [
        {
          test: /\.ts$/,
          use: () => {
            let loaders = [];
            if (process.env.NODE_ENV === 'production') {
              loaders.push("babel-loader");
            }
            loaders.push("ts-loader");
            return loaders
          },
          exclude: /node_modules/
        },
        {
          test: /\.js$/,
          loader: 'babel-loader',
          exclude: [helper.PATHS.node_modules]
        },
        {
          test: /\.(png|jpg|jpeg|gif|svg)(\?v=\d+\.\d+\.\d+)?$/,
          loader: 'file-loader',
          options: {
            name: '[folder]/[name].[ext]?[hash]'
          }
        },
        {
          test: /\.(eot|woff|woff2|ttf)(\?v=\d+\.\d+\.\d+)?$/,
          use: 'file-loader?name=[folder]/[name].[ext]',
        },
        {
          test: /\.scss$/,
          use: [{
            loader: "raw-loader"
          },
          {
            loader: 'postcss-loader',
          },
          {
            loader: "sass-loader"
          }
          ],
        },
        {
          test: /\.css$/,
          use: [{
            loader: 'raw-loader'
          },],
        },
        {
          test: /\.html$/,
          use: ['html-loader'],
        }
      ]
    },
    resolve: {
      modules: [helper.PATHS.src, helper.PATHS.node_modules],
      extensions: ['.ts', '.js', '.json']
    }
  }
}