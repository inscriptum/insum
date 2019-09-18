const { execSync, spawn } = require('child_process');
const RELEASE_MODE = !!process.env.RELEASE_MODE;

if (!RELEASE_MODE) {
  console.log('Run `npm run release` to publish the package');
  process.exitCode = 1;
} else {
  const branchName = execSync('git symbolic-ref --short HEAD', { encoding: 'utf8' }).trim();

  // prettier-ignore
  const cmd = branchName === 'master' ? 
    'npx lerna publish --conventional-graduate' : 
    'npx lerna publish --conventional-prerelease';

  console.log(`> ${cmd}`);
  spawn(cmd, { stdio: 'inherit', shell: true });
}
