module.exports = function (api) {

  api.cache(() => process.env.NODE_ENV);

  const presets = [
    [
      "@babel/preset-env",
      {
        // leave imports as they are
        "modules": false,
        "targets": {
          "esmodules": true
        },
        "useBuiltIns": 'usage',
        "debug": true
      }
    ]
  ];

  const plugins = [
    "@babel/plugin-syntax-dynamic-import"
  ];

  return {
    presets,
    plugins
  };
}