const { execSync } = require('child_process');

function installDependencies(dependencies, autoInstall = true) {
  return dependencies.map(dependency => {
    if (Array.isArray(dependency)) {
      const orDeps = installDependencies(dependency, false);
      if (orDeps.some(d => d)) {
        execSync(`npm i ${dependency[0]} -D && npm link @insum/webpack.config`, { stdio: 'inherit', shell: true });
        return true;
      }
    } else {
      try {
        console.log(111, require(dependency).version);
        return true;
      } catch (error) {
        console.log(222, error);
        
        if (autoInstall) {
          execSync(`npm i ${dependency} -D && npm link @insum/webpack.config`, { stdio: 'inherit', shell: true });
          return true;
        } else {
          return false;
        }
      }
    }
  });
}

module.exports = installDependencies;