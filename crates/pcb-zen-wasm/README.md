# pcb-zen-wasm

WebAssembly bindings for the PCB Zen language.

## Features

### CachingFileProvider

The WASM module uses a `CachingFileProvider` that:

- Caches files in memory for fast access
- Calls out to JavaScript to load missing files
- Supports lazy loading of files on demand

### JavaScript Interface

To use pcb-zen-wasm, you need to implement the following JavaScript functions:

#### `window.pcbZen.loadFile(path: string): string`

This function is called when the WASM module needs to load a file that's not in its cache.

- **Parameters**:
  - `path`: The file path to load (string)
- **Returns**:
  - The file content as a string
  - Or `"ERROR:" + error message` if the file doesn't exist or can't be loaded

Example implementation:

```javascript
window.pcbZen = window.pcbZen || {};

window.pcbZen.loadFile = function (path) {
  // Your file loading logic here
  try {
    // Load from your file system, server, etc.
    const content = loadFileFromSomewhere(path);
    return content;
  } catch (error) {
    return `ERROR:${error.message}`;
  }
};
```

#### `window.pcbZen.fetchRemoteSync(request: string): string`

This function handles remote fetching for load statements (e.g., loading from GitHub, packages, etc.).

- **Parameters**:
  - `request`: JSON-serialized fetch request
- **Returns**:
  - JSON-serialized fetch response
  - Or `"ERROR:" + error message` if the fetch fails

## Usage

```javascript
import init, { Module } from "pcb-zen-wasm";

async function main() {
  // Initialize the WASM module
  await init();

  // Set up the file loading function
  window.pcbZen = {
    loadFile: (path) => {
      // Your implementation
      return myFileSystem.readFile(path);
    },
  };

  // Create a module with initial files
  const files = {
    "/main.zen": "module MyModule:\n    pass",
  };

  const module = Module.fromFiles(
    JSON.stringify(files),
    "/main.zen",
    "MyModule"
  );

  // Files will be loaded on-demand as needed
  const result = module.evaluate("{}");
}
```

## File Loading Strategy

1. When a file is requested, the `CachingFileProvider` first checks its internal cache
2. If the file is not cached, it calls `window.pcbZen.loadFile(path)`
3. The loaded content is cached for future use
4. Subsequent requests for the same file are served from cache

This approach allows for:

- Lazy loading of files only when needed
- Integration with any JavaScript file system or storage mechanism
- Efficient caching to avoid repeated loads
