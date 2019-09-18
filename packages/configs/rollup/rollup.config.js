'use strict';

const resolvePlugin = require('rollup-plugin-node-resolve');
const terser = require('rollup-plugin-terser').terser;
const minifyLiteralsHTMLPlugin = require('rollup-plugin-minify-html-literals').default;

// `npm run build` -> `production` is true
// `npm run dev` -> `production` is false
const production = !process.env.ROLLUP_WATCH;

const dev = (input, file, plugins) => ({
  input,
  output: {
    file,
    format: 'iife', // immediately-invoked function expression — suitable for <script> tags
    sourcemap: true,
  },
  plugins: [resolvePlugin(), ...plugins],
});

const prod = (input, file, plugins) => ({
  input,
  output: {
    file,
    format: 'es',
  },
  plugins: [resolvePlugin(), terser(), minifyLiteralsHTMLPlugin(), ...plugins],
});

module.exports = {
  dev,
  prod,
  default: input => {
    return production ? prod(input) : dev(input);
  },
};
