import { fileURLToPath } from "node:url";

export const clientDirectory = fileURLToPath(new URL("../", import.meta.url));
const expoExecutable = fileURLToPath(new URL("../node_modules/.bin/expo", import.meta.url));

export async function runExpo(
  arguments_: readonly string[],
  environment: NodeJS.ProcessEnv = process.env,
): Promise<void> {
  const child = Bun.spawn({
    cmd: [expoExecutable, ...arguments_],
    cwd: clientDirectory,
    env: environment,
    stderr: "inherit",
    stdin: "inherit",
    stdout: "inherit",
  });
  const exitCode = await child.exited;
  if (exitCode !== 0) {
    throw new Error(`Expo ${arguments_.join(" ")} failed with status ${exitCode}`);
  }
}
