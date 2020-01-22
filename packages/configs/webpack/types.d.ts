export interface Helper {
  PATHS: {
    root: string;
    src: string;
    node_modules: string;
    dist: string;
    publicPath: string;
    outputPath: string;
  };
  TARGET: string;
  ENV: {
    devServerPort?: number;
  };
}
