const path = require("path");

// Sorry!
module.exports = {
  webpack: {
    alias: {
      "kicanvas/base": path.resolve(
        __dirname,
        "node_modules/kicanvas/src/base"
      ),
      "kicanvas/graphics": path.resolve(
        __dirname,
        "node_modules/kicanvas/src/graphics"
      ),
      "kicanvas/kicad": path.resolve(
        __dirname,
        "node_modules/kicanvas/src/kicad"
      ),
      "kicanvas/viewers": path.resolve(
        __dirname,
        "node_modules/kicanvas/src/viewers"
      ),
      "kicanvas/kicanvas": path.resolve(
        __dirname,
        "node_modules/kicanvas/src/kicanvas"
      ),
      "kicanvas/kc-ui": path.resolve(
        __dirname,
        "node_modules/kicanvas/src/kc-ui"
      ),
    },
    configure: (webpackConfig) => {
      // Remove ModuleScopePlugin which restricts imports outside of src/
      const scopePluginIndex = webpackConfig.resolve.plugins.findIndex(
        ({ constructor }) =>
          constructor && constructor.name === "ModuleScopePlugin"
      );
      if (scopePluginIndex >= 0) {
        webpackConfig.resolve.plugins.splice(scopePluginIndex, 1);
      }

      // Add .ts and .tsx extensions to resolve
      if (!webpackConfig.resolve.extensions.includes(".ts")) {
        webpackConfig.resolve.extensions.push(".ts", ".tsx");
      }

      // Find the existing TypeScript/Babel loader rule
      const tsRule = webpackConfig.module.rules.find(
        (rule) =>
          rule.oneOf &&
          rule.oneOf.some((r) => r.test && r.test.toString().includes("tsx"))
      );

      if (tsRule && tsRule.oneOf) {
        // Find the babel-loader rule for TypeScript
        const babelLoader = tsRule.oneOf.find(
          (rule) =>
            rule.test &&
            rule.test.toString().includes("tsx") &&
            rule.loader &&
            rule.loader.includes("babel-loader")
        );

        if (babelLoader) {
          // Update the include to also process kicanvas files
          if (Array.isArray(babelLoader.include)) {
            babelLoader.include.push(
              path.resolve(__dirname, "node_modules/kicanvas/src")
            );
          } else {
            babelLoader.include = [
              babelLoader.include,
              path.resolve(__dirname, "node_modules/kicanvas/src"),
            ];
          }
        }
      }

      return webpackConfig;
    },
  },
};
