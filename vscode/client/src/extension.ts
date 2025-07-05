/*
 * Copyright 2019 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

import { ExtensionContext } from "vscode";
import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient";
import * as path from "path";
import { TextDecoder } from "util";

let client: LanguageClient;

interface AdditionalClientSettings {
  enable_goto_definition: boolean;
}

/// Get a setting at the path, or throw an error if it's not set.
function requireSetting<T>(path: string): T {
  const ret: T = vscode.workspace.getConfiguration().get(path);
  if (ret == undefined) {
    throw new Error(`Setting "${path}" was not configured`);
  }
  return ret;
}

function additionalClientSettings(): AdditionalClientSettings {
  return {
    enable_goto_definition: vscode.workspace
      .getConfiguration()
      .get("zener.enableGotoDefinition", true),
  };
}

const ZENER_FILE_CONTENTS_METHOD = "zener/fileContents";
const ZENER_URI_SCHEME = "zener";

class ZenerFileContentsParams {
  constructor(public uri: String) {}
}

class ZenerFileContentsResponse {
  constructor(public contents?: string | null) {}
}

/// Ask the server for the contents of a zener: file
class ZenerFileHandler implements vscode.TextDocumentContentProvider {
  provideTextDocumentContent(
    uri: vscode.Uri,
    _token: vscode.CancellationToken
  ): vscode.ProviderResult<string> {
    if (client === undefined) {
      return null;
    } else {
      return client
        .sendRequest<ZenerFileContentsResponse>(
          ZENER_FILE_CONTENTS_METHOD,
          new ZenerFileContentsParams(uri.toString())
        )
        .then((response: ZenerFileContentsResponse) => {
          if (response.contents !== undefined && response.contents !== null) {
            return response.contents;
          } else {
            return null;
          }
        });
    }
  }
}

/* -------------------------------------------------------------------------
 * Schematic preview support (simplified from the atopile analyzer extension)
 * ------------------------------------------------------------------------- */

class SchematicPreviewDocument {
  public readonly uri: vscode.Uri;
  constructor(uri: vscode.Uri) {
    this.uri = uri;
  }
  dispose(): void {}
}

// Simple debounce implementation to limit how often expensive operations run.
function debounce<T extends (...args: any[]) => any>(
  fn: T,
  waitMs: number
): (...args: Parameters<T>) => void {
  let timeout: NodeJS.Timeout | undefined;
  return (...args: Parameters<T>): void => {
    if (timeout) {
      clearTimeout(timeout);
    }
    timeout = setTimeout(() => fn(...args), waitMs);
  };
}

class SchematicPreviewProvider {
  private static readonly viewType = "zener.preview";

  private updatePreviewDebounced: (
    document: vscode.TextDocument,
    webviewPanel: vscode.WebviewPanel
  ) => void;

  constructor(private readonly context: ExtensionContext) {
    // Debounce preview updates so we don't spam the LSP with expensive
    // viewer/getState requests while the user is actively typing.
    this.updatePreviewDebounced = debounce(
      (doc: vscode.TextDocument, panel: vscode.WebviewPanel) => {
        // Fire-and-forget; any errors are already handled inside updatePreview.
        this.updatePreview(doc, panel);
      },
      300 // milliseconds
    );
  }

  /**
   * For now we don't have a real net‑list. Send a stub so the React viewer can
   * render without errors. Later we will replace this with a real LSP request.
   */
  private async getNetlist(document: vscode.TextDocument): Promise<any> {
    // Guard against missing or not‑yet‑initialised language client.
    if (!client) {
      return {};
    }

    try {
      // The Zener LSP exposes a custom `viewer/getState` request that
      // returns the current evaluator state (net‑list) for a given source
      // file.  We forwards the current document URI so the server can locate
      // the correct cached state.

      console.error("sending viewer/getState request");

      const response: any = await client.sendRequest("viewer/getState", {
        uri: document.uri.toString(),
      });

      console.error("response", response);

      // The response shape is `{ state: <json|null> }` – unwrap if present.
      if (response && response.state) {
        return response.state;
      }
    } catch (err) {
      console.error("Failed to fetch netlist from LSP", err);
    }

    // Fallback to empty object so the React viewer can still initialise.
    return {};
  }

  private async updatePreview(
    document: vscode.TextDocument,
    webviewPanel: vscode.WebviewPanel
  ) {
    console.log("updatePreview");
    const netlist = await this.getNetlist(document);
    console.log("netlist", netlist);
    await webviewPanel.webview.postMessage({
      command: "update",
      netlist,
      currentFile: document.uri.fsPath,
    });
  }

  async resolveCustomTextEditor(
    document: vscode.TextDocument,
    webviewPanel: vscode.WebviewPanel,
    _token: vscode.CancellationToken
  ): Promise<void> {
    webviewPanel.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.file(
          path.join(this.context.extensionPath, "preview", "build")
        ),
      ],
    };

    const previewHtmlPath = vscode.Uri.file(
      path.join(this.context.extensionPath, "preview", "build", "index.html")
    );

    const previewHtml = await vscode.workspace.fs.readFile(previewHtmlPath);
    let htmlContent = new TextDecoder().decode(previewHtml);

    const buildDirUri = webviewPanel.webview.asWebviewUri(
      vscode.Uri.file(path.join(this.context.extensionPath, "preview", "build"))
    );

    // Rewrite asset URLs so they can be loaded by the webview.
    htmlContent = htmlContent
      .replace('<base href="/" />', `<base href="${buildDirUri}/" />`)
      .replace(
        /(src|href)="\/([^\"]*)"/g,
        (_m, attr, p) => `${attr}="${buildDirUri}/${p}"`
      )
      .replace(
        /(src|href)="\.\/([^\"]*)"/g,
        (_m, attr, p) => `${attr}="${buildDirUri}/${p}"`
      )
      .replace(
        /(manifest|icon|apple-touch-icon|shortcut icon)" href="([^\"]*)"/g,
        (_m, rel, p) => `${rel}" href="${buildDirUri}/${p}"`
      );

    webviewPanel.webview.html = htmlContent;

    // Respond to messages from the webview.
    webviewPanel.webview.onDidReceiveMessage((message) => {
      switch (message.command) {
        case "ready":
          this.updatePreviewDebounced(document, webviewPanel);
          break;
        case "error":
          vscode.window.showErrorMessage(message.text);
          break;
      }
    });

    // Refresh preview whenever *any* Zener file changes or is saved. This
    // ensures that updates in dependency files are reflected even when the
    // currently-viewed document itself is untouched.

    const changeSubscription = vscode.workspace.onDidChangeTextDocument((e) => {
      if (e.document.languageId === "zener") {
        this.updatePreviewDebounced(document, webviewPanel);
      }
    });

    const saveSubscription = vscode.workspace.onDidSaveTextDocument((doc) => {
      if (doc.languageId === "zener") {
        this.updatePreviewDebounced(document, webviewPanel);
      }
    });

    webviewPanel.onDidDispose(() => {
      changeSubscription.dispose();
      saveSubscription.dispose();
    });
  }

  async openCustomDocument(
    uri: vscode.Uri,
    _openContext: { backupId?: string },
    _token: vscode.CancellationToken
  ): Promise<SchematicPreviewDocument> {
    return new SchematicPreviewDocument(uri);
  }
}

/**
 * Run a shell command using VS Code's task system. The task runs hidden; if it
 * exits with a non-zero code we reveal the terminal so the user can inspect the
 * output.
 */
async function runShellCommand(cmd: string, cwd?: string): Promise<void> {
  const task = new vscode.Task(
    { type: "shell" },
    vscode.TaskScope.Workspace,
    cmd,
    "pcb",
    new vscode.ShellExecution(cmd, { cwd })
  );

  task.presentationOptions = {
    reveal: vscode.TaskRevealKind.Silent,
    focus: false,
    panel: vscode.TaskPanelKind.Dedicated,
    clear: true,
  };

  const exec = await vscode.tasks.executeTask(task);

  await new Promise<void>((resolve) => {
    const disp = vscode.tasks.onDidEndTaskProcess((ev) => {
      if (ev.execution === exec) {
        disp.dispose();
        resolve();
      }
    });
  });
}

export function activate(context: ExtensionContext) {
  // Make sure that any zener: URIs that come back from the LSP
  // are handled, and requested from the LSP.
  vscode.workspace.registerTextDocumentContentProvider(
    ZENER_URI_SCHEME,
    new ZenerFileHandler()
  );

  const lspPath: string = requireSetting("zener.lspPath");
  const args: [string] = requireSetting("zener.lspArguments");

  // Otherwise to spawn the server
  let serverOptions: ServerOptions = { command: lspPath, args: args };

  // Options to control the language client
  let clientOptions: LanguageClientOptions = {
    // Register the server for Zener documents
    documentSelector: [{ scheme: "file", language: "zener" }],
    initializationOptions: additionalClientSettings(),
  };

  // Create the language client and start the client.
  client = new LanguageClient(
    "Zener",
    "Zener language server",
    serverOptions,
    clientOptions
  );

  // Start the client. This will also launch the server.
  client.start();

  /* -------------------------- preview initialisation -------------------------- */

  // Status‑bar button to open the schematic preview.
  const schematicButton = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    100
  );
  schematicButton.text = "$(circuit-board)";
  schematicButton.command = "zener.openSchematic";
  schematicButton.tooltip = "Open schematic viewer";
  context.subscriptions.push(schematicButton);

  // Show/hide button based on active editor.
  const updateButtonVisibility = () => {
    const activeEditor = vscode.window.activeTextEditor;
    if (activeEditor && activeEditor.document.languageId === "zener") {
      schematicButton.show();
    } else {
      schematicButton.hide();
    }
  };
  updateButtonVisibility();
  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor(updateButtonVisibility)
  );

  // Register the custom editor provider for the preview.
  context.subscriptions.push(
    // Cast to any to handle older @types/vscode versions gracefully.
    (vscode.window as any).registerCustomEditorProvider(
      "zener.preview",
      new SchematicPreviewProvider(context)
    )
  );

  // Command to open the preview beside the current editor.
  context.subscriptions.push(
    vscode.commands.registerCommand("zener.openSchematic", async () => {
      const activeEditor = vscode.window.activeTextEditor;
      if (!activeEditor || activeEditor.document.languageId !== "zener") {
        vscode.window.showErrorMessage("Please open a Zener file first");
        return;
      }

      const panel = vscode.window.createWebviewPanel(
        "zener.preview",
        "Schematic Preview",
        vscode.ViewColumn.Beside,
        {
          enableScripts: true,
          retainContextWhenHidden: true,
          localResourceRoots: [
            vscode.Uri.file(
              path.join(context.extensionPath, "preview", "build")
            ),
          ],
        }
      );

      const provider = new SchematicPreviewProvider(context);
      await provider.resolveCustomTextEditor(
        activeEditor.document,
        panel,
        undefined as any // cancellation token not required
      );
    })
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("zener.runLayout", async () => {
      const activeEditor = vscode.window.activeTextEditor;
      if (!activeEditor) {
        vscode.window.showErrorMessage(
          "Please open a file first to run layout"
        );
        return;
      }

      const pcbBinary = lspPath;
      const targetPath = activeEditor.document.uri.fsPath;

      const shellCmd = `"${pcbBinary}" layout "${targetPath}"`;

      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: "Updating layout...",
          cancellable: false,
        },
        () => runShellCommand(shellCmd, path.dirname(targetPath))
      );
    })
  );
}

export function deactivate(): Thenable<void> | undefined {
  if (!client) {
    return undefined;
  }
  return client.stop();
}
