import { dev, prod } from '@insum/rollup.config';

import typescriptPlugin from 'rollup-plugin-typescript';
import postcss from 'rollup-plugin-postcss';
var atImport = require('postcss-import');


// `npm run build` -> `production` is true
// `npm run dev` -> `production` is false
const production = !process.env.ROLLUP_WATCH;

const plugins = [
  typescriptPlugin(),
  postcss({
    inject: false,
    extensions: ['.css', '.pcss', '.scss'],
    plugins: [atImport()]
  })
];

export default production
  ? prod('./src/index.ts', './lib/index.js', plugins)
  : dev('./public/index.ts', './public/index.js', plugins);
