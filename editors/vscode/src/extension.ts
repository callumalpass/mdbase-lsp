import * as path from "path";
import * as fs from "fs";
import {
  commands,
  ExtensionContext,
  OutputChannel,
  workspace,
  window,
  WorkspaceFolder,
} from "vscode";
import {
  ExecuteCommandRequest,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let validationOutput: OutputChannel | undefined;

function resolveServerPath(extensionPath: string): string | undefined {
  const config = workspace.getConfiguration("mdbase");
  const userPath: string = config.get("serverPath", "");
  if (userPath) {
    if (fs.existsSync(userPath) && fs.statSync(userPath).isFile()) {
      return userPath;
    }
    window.showErrorMessage(
      `mdbase.serverPath is set but invalid: ${userPath}`
    );
    return undefined;
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

async function pickWorkspaceFolder(): Promise<WorkspaceFolder | undefined> {
  const folders = workspace.workspaceFolders;
  if (!folders || folders.length === 0) {
    return undefined;
  }
  if (folders.length === 1) {
    return folders[0];
  }

  const activeUri = window.activeTextEditor?.document.uri;
  if (activeUri) {
    const owningFolder = workspace.getWorkspaceFolder(activeUri);
    if (owningFolder) {
      return owningFolder;
    }
  }

  const picked = await window.showWorkspaceFolderPick({
    placeHolder: "Select a workspace folder for mdbase type discovery",
  });
  return picked;
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

  try {
    await client.start();
  } catch (e) {
    client = undefined;
    window.showErrorMessage(
      `Failed to start mdbase-lsp: ${e instanceof Error ? e.message : String(e)}`
    );
    return;
  }

  context.subscriptions.push(
    commands.registerCommand("mdbase.createFile", async () => {
      if (!client) {
        window.showErrorMessage("mdbase: no client");
        return;
      }

      // Discover available types from _types/ folder
      const folder = await pickWorkspaceFolder();
      const typesDir = folder ? path.join(folder.uri.fsPath, "_types") : undefined;
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

      // Query typeInfo for prompt fields
      let promptFields: Array<{
        name: string;
        type: string;
        description?: string;
        values?: string[];
      }> = [];
      try {
        const typeInfoResult = await client.sendRequest(
          ExecuteCommandRequest.type,
          {
            command: "mdbase.typeInfo",
            arguments: [{ type: typeName }],
          }
        );
        promptFields =
          (typeInfoResult as Record<string, unknown>)?.prompt_fields as typeof promptFields ?? [];
      } catch (e) {
        window.showWarningMessage(
          `mdbase: typeInfo failed: ${e instanceof Error ? e.message : e}`
        );
      }

      const filePath = await window.showInputBox({
        prompt: "File path (blank to auto-generate)",
        placeHolder: "e.g. notes/my-note.md (leave empty to auto-generate)",
      });
      if (filePath === undefined) {
        return;
      }

      // Prompt for each required field
      const frontmatter: Record<string, string> = {};
      for (const field of promptFields) {
        let value: string | undefined;
        const label = field.description
          ? `${field.name} (${field.description})`
          : field.name;

        if (field.values && field.values.length > 0) {
          value = await window.showQuickPick(field.values, {
            placeHolder: label,
          });
        } else {
          value = await window.showInputBox({ prompt: label });
        }
        if (value === undefined) {
          return;
        }
        if (value !== "") {
          frontmatter[field.name] = value;
        }
      }

      // Build args — only include path if non-empty
      const createArgs: Record<string, unknown> = {
        type: typeName,
        frontmatter,
      };
      if (filePath !== "") {
        createArgs.path = filePath;
      }

      await client.sendRequest(ExecuteCommandRequest.type, {
        command: "mdbase.createFile",
        arguments: [createArgs],
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
        if (!validationOutput) {
          validationOutput = window.createOutputChannel("mdbase validation");
          context.subscriptions.push(validationOutput);
        }
        validationOutput.clear();
        validationOutput.appendLine(JSON.stringify(result, null, 2));
        validationOutput.show();
      } else {
        window.showInformationMessage("mdbase: collection is valid");
      }
    })
  );

  context.subscriptions.push(
    commands.registerCommand("mdbase.queryCollection", async () => {
      if (!client) {
        return;
      }
      const query = await window.showInputBox({
        prompt: "Collection query",
        placeHolder: "examples: type:zettel, tag:project, title:roadmap",
      });
      if (query === undefined) {
        return;
      }

      const result = await client.sendRequest(ExecuteCommandRequest.type, {
        command: "mdbase.queryCollection",
        arguments: [{ query }],
      });
      if (!validationOutput) {
        validationOutput = window.createOutputChannel("mdbase validation");
        context.subscriptions.push(validationOutput);
      }
      validationOutput.clear();
      validationOutput.appendLine(JSON.stringify(result, null, 2));
      validationOutput.show();
    })
  );
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
  }
}
