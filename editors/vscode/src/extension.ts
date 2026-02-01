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
      window.showInformationMessage("mdbase: createFile command invoked");
      if (!client) {
        window.showErrorMessage("mdbase: no client");
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
