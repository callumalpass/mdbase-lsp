import * as path from "path";
import * as fs from "fs";
import {
  commands,
  ExtensionContext,
  workspace,
  window,
} from "vscode";
import {
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
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

  context.subscriptions.push(client);

  await client.start();

  context.subscriptions.push(
    commands.registerCommand("mdbase.createFile", async () => {
      if (!client) {
        return;
      }

      // Discover available types from _types/ folder
      const folders = workspace.workspaceFolders;
      const typesDir = folders?.[0]
        ? path.join(folders[0].uri.fsPath, "_types")
        : undefined;
      let typeNames: string[] = [];
      if (typesDir && fs.existsSync(typesDir)) {
        typeNames = fs
          .readdirSync(typesDir)
          .filter((f) => f.endsWith(".md"))
          .map((f) => f.replace(/\.md$/, ""));
      }

      let typeName: string | undefined;
      if (typeNames.length > 0) {
        typeName = await window.showQuickPick(typeNames, {
          placeHolder: "Select a type",
        });
      } else {
        typeName = await window.showInputBox({
          prompt: "Type name",
          placeHolder: "e.g. zettel",
        });
      }
      if (!typeName) {
        return;
      }

      const filePath = await window.showInputBox({
        prompt: "File path (relative to collection root)",
        placeHolder: "e.g. notes/my-note.md",
      });
      if (!filePath) {
        return;
      }

      await client.sendRequest(ExecuteCommandRequest.type, {
        command: "mdbase.createFile",
        arguments: [{ type: typeName, path: filePath, frontmatter: {} }],
      });
    })
  );

  context.subscriptions.push(
    commands.registerCommand("mdbase.validateCollection", async () => {
      if (!client) {
        return;
      }

      const result = await client.sendRequest(ExecuteCommandRequest.type, {
        command: "mdbase.validateCollection",
        arguments: [],
      });

      if (result) {
        const output = window.createOutputChannel("mdbase validation");
        output.clear();
        output.appendLine(JSON.stringify(result, null, 2));
        output.show();
      } else {
        window.showInformationMessage("mdbase: collection is valid");
      }
    })
  );
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
  }
}
