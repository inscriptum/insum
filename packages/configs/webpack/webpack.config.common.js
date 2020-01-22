/** @typedef {import('./types').Helper} Helper */

/**
 * @param {Helper} helper
 */
module.exports = function(helper) {
  return {
    mode: helper.TARGET,
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
