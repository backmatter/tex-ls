import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { defineConfig } from "vite-plus";

// Bundled packages ship without their own directories, so collect their license texts.
const thirdPartyNotices = {
  name: "third-party-notices",
  generateBundle(_: unknown, bundle: Record<string, { moduleIds?: string[] }>) {
    const packages = new Map<string, string>();
    for (const chunk of Object.values(bundle)) {
      for (const id of chunk.moduleIds ?? []) {
        if (!id.includes("node_modules")) continue;
        // Skip nested manifests such as dist/esm/package.json that only set the module type.
        let directory = dirname(id);
        let manifest: { name?: string; version?: string } = {};
        for (; !manifest.name; directory = dirname(directory)) {
          const path = join(directory, "package.json");
          if (existsSync(path)) manifest = JSON.parse(readFileSync(path, "utf8"));
          if (manifest.name) break;
        }
        const license = readdirSync(directory).find((name) => /^(license|licence)/i.test(name));
        if (!license) throw new Error(`${manifest.name} has no license file to bundle.`);
        packages.set(`${manifest.name}@${manifest.version}`, readFileSync(join(directory, license), "utf8").trim());
      }
    }
    const notices = [...packages].sort(([a], [b]) => a.localeCompare(b))
      .map(([name, text]) => `${name}\n\n${text}`).join(`\n\n${"-".repeat(72)}\n\n`);
    (this as any).emitFile({
      type: "asset",
      fileName: "THIRD-PARTY-NOTICES.txt",
      source: `tex-ls VS Code extension: bundled dependency notices\n\n${notices}\n`,
    });
  },
};

// VS Code loads one CommonJS bundle; only the `vscode` module comes from the host.
export default defineConfig({
  pack: {
    entry: { extension: "src/extension.ts" },
    format: "cjs",
    platform: "node",
    target: "node20",
    outDir: "dist",
    fixedExtension: false,
    dts: false,
    deps: {
      neverBundle: ["vscode"],
      onlyBundle: [
        "vscode-languageclient", "vscode-jsonrpc", "vscode-languageserver-protocol",
        "vscode-languageserver-types", "vscode-languageserver-textdocument",
        "minimatch", "@isaacs/nonrecursive-glob-match", "brace-expansion", "balanced-match", "semver",
      ],
    },
    plugins: [thirdPartyNotices],
  },
});
