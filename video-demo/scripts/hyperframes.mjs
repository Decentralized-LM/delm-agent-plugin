import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = fileURLToPath(new URL("../", import.meta.url));
const env = {
  ...process.env,
  HYPERFRAMES_NO_UPDATE_CHECK: "1",
  HYPERFRAMES_NO_TELEMETRY: "1",
};
const chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
if (
  !env.HYPERFRAMES_BROWSER_PATH &&
  process.platform === "darwin" &&
  existsSync(chrome)
)
  env.HYPERFRAMES_BROWSER_PATH = chrome;
const child = spawn(
  path.join(root, "node_modules/.bin/hyperframes"),
  process.argv.slice(2),
  { cwd: root, env, stdio: "inherit" },
);
child.on("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.on("exit", (code) => {
  process.exitCode = code ?? 1;
});
