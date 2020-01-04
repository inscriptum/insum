// A publish process must to be started only a command 'npm run release'
if (process.env.RELEASE_MODE !== 'true') {
  console.warn('Run `npm run release` to publish the package!');
  process.exitCode = 1;
}
