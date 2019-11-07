module.exports = function(helper) {
  return {
    mode: process.env.NODE_ENV,
    output: {
      path: helper.PATHS.dist,
      publicPath: '/',
      filename: '[name].js',
    },
    resolve: {
      modules: [helper.PATHS.src, helper.PATHS.node_modules],
      extensions: ['.ts', '.js', '.json'],
    },
  };
};
