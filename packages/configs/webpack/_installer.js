const { execSync } = require('child_process');

function installDependencies(dependencies, autoInstall = true) {
  return dependencies.map(dependency => {
    if (Array.isArray(dependency)) {
      const orDeps = installDependencies(dependency, false);
      if (!orDeps.some(d => d)) {
        // eslint-disable-next-line no-console
        console.log(`Try to install ${dependency[0]}`);
        execSync(`npm i ${dependency[0]} -D`, { stdio: 'inherit', shell: true });
        return true;
      }
    } else {
      try {
        // eslint-disable-next-line no-console
        console.log(`${dependency} version ${require(`${dependency}/package.json`).version}`);
        return true;
      } catch (error) {
        if (autoInstall) {
          // eslint-disable-next-line no-console
          console.log(`Try to install ${dependency}`);
          execSync(`npm i ${dependency} -D`, { stdio: 'inherit', shell: true });
          return true;
        } else {
          return false;
        }
      }
    }
  });
}

module.exports = installDependencies;
