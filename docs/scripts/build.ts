// Builds every documentation version into dist/, as planned by `planBuilds`:
// each one is the current site (config, theme, components) around the content
// of its tag, or of the working tree for the development version.
import { execFileSync } from "node:child_process";
import { cpSync, rmSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { planBuilds, SRC_DIR_ENV, VERSION_ENV, VERSIONS_ENV } from "../src/docs-versions.ts";

const docsDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoDir = dirname(docsDir);
const distDir = join(docsDir, "dist");
// Inside the project, so a staged version resolves the same node_modules.
const stagingDir = join(docsDir, ".versions");
const contentPath = "docs/src/content/docs";

function log(event: string, fields: Record<string, string>): void {
	const pairs = Object.entries(fields).map(([key, value]) => `${key}=${JSON.stringify(value)}`);
	process.stderr.write(`${[event, ...pairs].join(" ")}\n`);
}

function hasDocs(tag: string): boolean {
	try {
		execFileSync("git", ["cat-file", "-e", `${tag}:${contentPath}`], {
			cwd: repoDir,
			stdio: "ignore",
		});
		return true;
	} catch {
		return false;
	}
}

/**
 * Lays the current `src/` out under `.versions/<label>/`, mirroring the
 * repository so the content's relative paths (the logo) still resolve, and
 * swaps in the tag's content.
 */
function stage(label: string, ref: string): string {
	const root = join(stagingDir, label);
	cpSync(join(docsDir, "src"), join(root, "docs", "src"), { recursive: true });
	cpSync(join(repoDir, "logo.svg"), join(root, "logo.svg"));
	rmSync(join(root, contentPath), { recursive: true });
	const archive = execFileSync("git", ["archive", ref, contentPath], {
		cwd: repoDir,
		maxBuffer: 256 * 1024 * 1024,
	});
	execFileSync("tar", ["-x", "-C", root], { input: archive });
	return `./${relative(docsDir, join(root, "docs", "src"))}`;
}

const tags = execFileSync("git", ["tag", "--list", "v*"], { cwd: repoDir, encoding: "utf8" })
	.split("\n")
	.filter((tag) => tag !== "" && hasDocs(tag));
const builds = planBuilds(tags);
const versions = JSON.stringify(builds.map(({ version }) => version));
const outRoot = join(stagingDir, "out");

try {
	rmSync(stagingDir, { recursive: true, force: true });
	rmSync(distDir, { recursive: true, force: true });
	for (const { version, ref } of builds) {
		log("docs.build", { version: version.label, ref: ref ?? "working tree", path: version.path });

		const env: NodeJS.ProcessEnv = {
			...process.env,
			[VERSIONS_ENV]: versions,
			[VERSION_ENV]: version.label,
		};
		if (ref !== null) env[SRC_DIR_ENV] = stage(version.label, ref);

		const outDir = join(outRoot, version.label);
		// `--force`: the content layer cache is shared by every version built
		// here, and would otherwise serve the previous one's pages.
		execFileSync("npx", ["astro", "build", "--force", "--outDir", outDir], {
			cwd: docsDir,
			stdio: "inherit",
			env,
		});
		cpSync(outDir, join(distDir, version.path), { recursive: true });
	}
	log("docs.done", {
		versions: builds.map(({ version }) => version.label).join(","),
		dist: distDir,
	});
} finally {
	rmSync(stagingDir, { recursive: true, force: true });
}
