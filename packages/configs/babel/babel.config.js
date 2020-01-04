module.exports = function(api) {
  // cache if function result didn't change
  api.cache(() => process.env.NODE_ENV);

  const presets = [
    [
      '@babel/preset-env',
      {
        // leave imports as they are
        modules: false,
        targets: {
          esmodules: true,
        },
        // adds specific imports for polyfills when they are used in each file
        useBuiltIns: 'usage',
      },
    ],
  ];

  const plugins = ['@babel/plugin-syntax-dynamic-import'];

  return {
    presets,
    plugins,
  };
};
