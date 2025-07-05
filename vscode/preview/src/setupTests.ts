// jest-dom adds custom jest matchers for asserting on DOM nodes.
// allows you to do things like:
// expect(element).toHaveTextContent(/react/i)
// learn more: https://github.com/testing-library/jest-dom
import "@testing-library/jest-dom";

// Mock VSCode web components bundle (ESM syntax causes Jest parse errors)
jest.mock("@vscode-elements/elements/dist/bundled.js", () => ({}));

// Polyfill ResizeObserver used by @xyflow/react inside jsdom test environment
// @ts-ignore
global.ResizeObserver =
  global.ResizeObserver ||
  class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
