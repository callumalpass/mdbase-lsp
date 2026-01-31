import * as path from "path";
import * as fs from "fs";
import {
  ExtensionContext,
  workspace,
  commands,
  window,
} from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  ExecuteCommandRequest,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

function resolveServerPath(extensionPath: string): string | undefined {
  const config = workspace.getConfiguration("mdbase");
  const userPath: string = config.get("serverPath", "");
  if (userPath) {
    return userPath;
  }

  const ext = process.platform === "win32" ? ".exe" : "";
  const bundled = path.join(extensionPath, "server", `mdbase-lsp${ext}`);
  if (fs.existsSync(bundled)) {
    return bundled;
  }

  const root = path.resolve(extensionPath, "..", "..");
  const candidates = [
    path.join(root, "target", "release", `mdbase-lsp${ext}`),
    path.join(root, "target", "debug", `mdbase-lsp${ext}`),
  ];
  return candidates.find((p) => fs.existsSync(p));
}

export async function activate(context: ExtensionContext): Promise<void> {
  const serverPath = resolveServerPath(context.extensionPath);
  if (!serverPath) {
    window.showErrorMessage(
      "mdbase-lsp binary not found. Set mdbase.serverPath or build the project first."
    );
    return;
  }

  const config = workspace.getConfiguration("mdbase");
  const logLevel: string = config.get("logLevel", "info");

  const serverOptions: ServerOptions = {
    command: serverPath,
    args: [],
    options: {
      env: { ...process.env, RUST_LOG: logLevel },
    },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "markdown" }],
  };

  client = new LanguageClient(
    "mdbase",
    "mdbase-lsp",
    serverOptions,
    clientOptions
  );

  const registerCommand = (command: string) =>
    commands.registerCommand(command, async () => {
      if (!client) {
        return;
      }
      await client.sendRequest(ExecuteCommandRequest.type, {
        command,
        arguments: [],
      });
    });

  context.subscriptions.push(
    registerCommand("mdbase.createFile"),
    registerCommand("mdbase.validateCollection")
  );

  await client.start();
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
  }
}
