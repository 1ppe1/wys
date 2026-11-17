import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

function serverPath(): string {
  return vscode.workspace.getConfiguration("wys").get<string>("path") || "wys";
}

async function start(): Promise<void> {
  const command = serverPath();
  const serverOptions: ServerOptions = { command, args: ["lsp"] };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "wys" }],
  };
  client = new LanguageClient("wys", "wys", serverOptions, clientOptions);
  try {
    await client.start();
  } catch (err) {
    client = undefined;
    void vscode.window.showErrorMessage(
      `wys: could not start the language server (\`${command} lsp\`). ` +
        "Install wys or set `wys.path`. " +
        String(err)
    );
  }
}

async function stop(): Promise<void> {
  const running = client;
  client = undefined;
  if (running) {
    await running.stop();
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  context.subscriptions.push(
    vscode.commands.registerCommand("wys.restartServer", async () => {
      await stop();
      await start();
    }),
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("wys.path")) {
        await stop();
        await start();
      }
    })
  );
  await start();
}

export function deactivate(): Promise<void> {
  return stop();
}
