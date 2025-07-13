const path = require("path");
const CopyWebpackPlugin = require("copy-webpack-plugin");

// Sorry!
module.exports = {
  jest: {
    configure: (jestConfig) => {
      // Update module name mapper
      jestConfig.moduleNameMapper = {
        ...jestConfig.moduleNameMapper,
        "^kicanvas/base/(.*)$": "<rootDir>/node_modules/kicanvas/src/base/$1",
        "^kicanvas/graphics/(.*)$":
          "<rootDir>/node_modules/kicanvas/src/graphics/$1",
        "^kicanvas/kicad/(.*)$": "<rootDir>/node_modules/kicanvas/src/kicad/$1",
        "^kicanvas/viewers/(.*)$":
          "<rootDir>/node_modules/kicanvas/src/viewers/$1",
        "^kicanvas/kicanvas/(.*)$":
          "<rootDir>/node_modules/kicanvas/src/kicanvas/$1",
        "^kicanvas/kc-ui/(.*)$": "<rootDir>/node_modules/kicanvas/src/kc-ui/$1",
      };

      // Update transform ignore patterns to include kicanvas and libavoid-js
      jestConfig.transformIgnorePatterns = [
        "[/\\\\]node_modules[/\\\\](?!(kicanvas|@vscode-elements|libavoid-js)[/\\\\]).+\\.(js|jsx|mjs|cjs|ts|tsx)$",
      ];

      // Add transform for TypeScript files
      jestConfig.transform = {
        ...jestConfig.transform,
        "^.+\\.(ts|tsx)$": require.resolve(
          "react-scripts/config/jest/babelTransform.js"
        ),
      };

      // Ensure TypeScript extensions are included
      jestConfig.moduleFileExtensions = [
        "ts",
        "tsx",
        "js",
        "jsx",
        "json",
        "node",
      ];

      return jestConfig;
    },
  },
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

      // Add CopyWebpackPlugin to copy WASM files
      webpackConfig.plugins.push(
        new CopyWebpackPlugin({
          patterns: [
            {
              from: path.resolve(
                __dirname,
                "node_modules/libavoid-js/dist/libavoid.wasm"
              ),
              to: "static/js/libavoid.wasm",
            },
          ],
        })
      );

      return webpackConfig;
    },
  },
  devServer: {
    setupMiddlewares: (middlewares, devServer) => {
      // Ensure WASM files are served with correct MIME type
      devServer.app.get("*.wasm", (req, res, next) => {
        res.type("application/wasm");
        next();
      });
      return middlewares;
    },
  },
};
