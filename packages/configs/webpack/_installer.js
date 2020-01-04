const { execSync } = require('child_process');

const logger = {
  info: function(...args) {
    if (process.env.NODE_ENV !== 'production') {
      // eslint-disable-next-line no-console
      console.info.apply(this, args);
    }
  },
  warn: function(...args) {
    console.warn.apply(this, args);
  },
};

function installDependencies(dependencies, autoInstall = true) {
  return dependencies.map(dependency => {
    if (Array.isArray(dependency)) {
      const orDeps = installDependencies(dependency, false);
      if (!orDeps.some(d => d)) {
        logger.warn(`Try to install ${dependency[0]}`);
        execSync(`npm i ${dependency[0]} -D`, { stdio: 'inherit', shell: true });
        return true;
      }
    } else {
      try {
        const version = require(`${dependency}/package.json`).version;
        logger.info(`${dependency} version ${version}`);
        return true;
      } catch (error) {
        if (autoInstall) {
          logger.warn(`Try to install ${dependency}`);
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
