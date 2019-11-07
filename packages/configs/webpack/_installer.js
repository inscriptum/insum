const { execSync } = require('child_process');

function installDependencies(dependencies, autoInstall = true) {
  return dependencies.map(dependency => {
    if (Array.isArray(dependency)) {
      const orDeps = installDependencies(dependency, false);
      if (orDeps.some(d => d)) {
        execSync(`npm i ${dependency[0]} -D`, { stdio: 'inherit', shell: true });
        return true;
      }
    } else {
      try {
        // eslint-disable-next-line no-console
        console.log(`${dependency} version ${require(dependency).version}`);
        return true;
      } catch (error) {
        if (autoInstall) {
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
