module.exports = {
  module_rule: {
    typescript: {
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
    javascript: {
      test: /\.js$/,
      loader: 'babel-loader',
      exclude: /node_modules/
    },
    image: {
      test: /\.(png|jpg|jpeg|gif|svg)(\?v=\d+\.\d+\.\d+)?$/,
      loader: 'file-loader',
      options: {
        name: '[folder]/[name].[ext]?[hash]'
      }
    },
    font: {
      test: /\.(eot|woff|woff2|ttf)(\?v=\d+\.\d+\.\d+)?$/,
      use: 'file-loader?name=[folder]/[name].[ext]',
    },
    scss_raw: {
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
    css_raw: {
      test: /\.css$/,
      use: [{
        loader: 'raw-loader'
      },],
    },
    html: {
      test: /\.html$/,
      use: ['html-loader'],
    }
  }
}