// The documentation versions a build knows about. `scripts/build.ts` plans them
// and hands them to each `astro build` through the environment; the config and
// the version picker read them back from here.
import { z } from "astro/zod";

const versionSchema = z.object({
	label: z.string().min(1),
	path: z.string().startsWith("/").endsWith("/"),
	latest: z.boolean(),
});

export type DocsVersion = z.infer<typeof versionSchema>;

export interface PlannedBuild {
	version: DocsVersion;
	/** The tag whose content is built, or `null` for the working tree. */
	ref: string | null;
}

export interface BuildTarget {
	versions: DocsVersion[];
	current: DocsVersion;
	/** Where the sources of a tagged version were staged; `null` for the working tree. */
	srcDir: string | null;
}

export const VERSIONS_ENV = "OAS2MCP_DOCS_VERSIONS";
export const VERSION_ENV = "OAS2MCP_DOCS_VERSION";
export const SRC_DIR_ENV = "OAS2MCP_DOCS_SRC_DIR";

const STABLE_TAG = /^v(\d+)\.(\d+)\.(\d+)$/;

/**
 * The latest release is served at the root, the working tree under `/next/`,
 * and every older minor release, at its last patch, under `/vX.Y/`. Without
 * any release, the working tree takes the root.
 */
export function planBuilds(tags: readonly string[]): PlannedBuild[] {
	const lastPatchByMinor = new Map<
		string,
		{ tag: string; patch: number; minor: [number, number] }
	>();
	for (const tag of tags) {
		const match = STABLE_TAG.exec(tag);
		if (!match) continue;
		const [, major, minor, patch] = match.map(Number) as [number, number, number, number];
		const key = `v${major}.${minor}`;
		const known = lastPatchByMinor.get(key);
		if (!known || patch > known.patch) {
			lastPatchByMinor.set(key, { tag, patch, minor: [major, minor] });
		}
	}

	const releases = [...lastPatchByMinor.entries()].sort(
		([, a], [, b]) => b.minor[0] - a.minor[0] || b.minor[1] - a.minor[1],
	);
	if (releases.length === 0) {
		return [{ version: { label: "next", path: "/", latest: false }, ref: null }];
	}

	return [
		...releases.slice(0, 1).map(([label, { tag }]) => ({
			version: { label, path: "/", latest: true },
			ref: tag,
		})),
		{ version: { label: "next", path: "/next/", latest: false }, ref: null },
		...releases.slice(1).map(([label, { tag }]) => ({
			version: { label, path: `/${label}/`, latest: false },
			ref: tag,
		})),
	];
}

/** `null` for a plain, unversioned build such as `astro dev`. */
export function readBuildTarget(env: Record<string, string | undefined>): BuildTarget | null {
	const rawVersions = env[VERSIONS_ENV];
	const label = env[VERSION_ENV];
	if (rawVersions === undefined || label === undefined) return null;

	const versions = z.array(versionSchema).parse(JSON.parse(rawVersions));
	const current = versions.find((version) => version.label === label);
	if (!current) {
		throw new Error(`${VERSION_ENV}=${label} is not one of the versions in ${VERSIONS_ENV}`);
	}
	return { versions, current, srcDir: env[SRC_DIR_ENV] ?? null };
}

/** Tells a model reading `llms.txt` which version it describes, and where the others are. */
export function llmsVersionNote(target: BuildTarget | null, site: string): string | undefined {
	if (!target || target.versions.length < 2) return undefined;
	const { current, versions } = target;
	const others = versions
		.filter((version) => version !== current)
		.map((version) => `${version.label}: ${new URL(`${version.path}llms.txt`, site).href}`)
		.join(", ");
	const which = current.latest
		? `the latest release, ${current.label}`
		: current.label === "next"
			? "the unreleased development version"
			: `${current.label}, an older release`;
	return `This file documents ${which}. Other versions have their own (${others}).`;
}
