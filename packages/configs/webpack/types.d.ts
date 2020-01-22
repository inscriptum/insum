export interface Helper {
  PATHS: {
    root: string;
    src: string;
    node_modules: string;
    dist: string;
    publicPath: string;
    outputPath: string;
  };
  ENV: {
    isDevMode: boolean;
    target?: string;
    devServerPort?: number;
  };
}
