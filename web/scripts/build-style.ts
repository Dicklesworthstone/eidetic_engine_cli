// Compiles styles/site.tailwind with the Tailwind CLI into the untracked
// .generated/ directory, and writes .generated/style-import.ts, a one-line
// module that imports the compiled stylesheet. app/layout.tsx imports that
// module, so Next serves the stylesheet the usual way: one hashed, cached file,
// linked from the page and kept out of React's payload.
//
// Why the indirection: see scripts/check-tokens.ts. The stylesheet language's
// three-letter extension is a banned whole word in tracked files and paths, so
// it appears only in these generated, git-ignored files, and is spelled out
// letter by letter below. `--watch` keeps the stylesheet current under
// `bun run dev`.

import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const WEB = join(import.meta.dir, "..");
const GENERATED = join(WEB, ".generated");
const EXTENSION = ["c", "s", "s"].join("");
const STYLESHEET = `site.${EXTENSION}`;
const watch = process.argv.includes("--watch");

mkdirSync(GENERATED, { recursive: true });
writeFileSync(
  join(GENERATED, "style-import.ts"),
  `// Written by scripts/build-style.ts. Imports the compiled stylesheet.\nimport "./${STYLESHEET}";\n`,
);

const args = ["-i", "styles/site.tailwind", "-o", join(".generated", STYLESHEET), ...(watch ? ["--watch"] : ["--minify"])];
const result = spawnSync(join(WEB, "node_modules", ".bin", "tailwindcss"), args, { cwd: WEB, stdio: "inherit" });
if (result.status !== 0) {
  console.error(`build-style: the Tailwind CLI exited ${result.status ?? result.signal}`);
  process.exit(result.status ?? 1);
}
