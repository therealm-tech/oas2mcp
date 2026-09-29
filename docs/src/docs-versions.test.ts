import assert from "node:assert/strict";
import { describe, it } from "node:test";
import {
	llmsVersionNote,
	planBuilds,
	readBuildTarget,
	SRC_DIR_ENV,
	VERSION_ENV,
	VERSIONS_ENV,
} from "./docs-versions.ts";

const plan = (tags: string[]) => planBuilds(tags).map(({ version, ref }) => [version.path, ref]);

describe("planBuilds", () => {
	it("serves the working tree at the root before any release", () => {
		assert.deepEqual(plan([]), [["/", null]]);
	});

	it("serves the latest release at the root and the working tree under /next/", () => {
		assert.deepEqual(plan(["v0.9.0", "v0.10.0", "v0.10.1"]), [
			["/", "v0.10.1"],
			["/next/", null],
			["/v0.9/", "v0.9.0"],
		]);
	});

	it("keeps the last patch of each minor, compared as numbers", () => {
		assert.deepEqual(plan(["v1.2.9", "v1.2.10", "v1.1.3"]), [
			["/", "v1.2.10"],
			["/next/", null],
			["/v1.1/", "v1.1.3"],
		]);
	});

	it("ignores pre-releases and other tags", () => {
		assert.deepEqual(plan(["v1.0.0", "v1.1.0-rc1", "chart-2.0.0"]), [
			["/", "v1.0.0"],
			["/next/", null],
		]);
	});

	it("marks only the root release as latest", () => {
		const latest = planBuilds(["v1.0.0", "v0.9.0"]).filter(({ version }) => version.latest);
		assert.deepEqual(
			latest.map(({ version }) => version.label),
			["v1.0"],
		);
	});
});

describe("readBuildTarget", () => {
	const versions = JSON.stringify(planBuilds(["v1.0.0"]).map(({ version }) => version));

	it("is null for an unversioned build", () => {
		assert.equal(readBuildTarget({}), null);
	});

	it("finds the version being built", () => {
		const target = readBuildTarget({ [VERSIONS_ENV]: versions, [VERSION_ENV]: "next" });
		assert.equal(target?.current.path, "/next/");
		assert.equal(target?.versions.length, 2);
		assert.equal(target?.srcDir, null);
	});

	it("carries the staged sources of a tagged version", () => {
		const target = readBuildTarget({
			[VERSIONS_ENV]: versions,
			[VERSION_ENV]: "v1.0",
			[SRC_DIR_ENV]: "./.versions/v1.0/docs/src",
		});
		assert.equal(target?.srcDir, "./.versions/v1.0/docs/src");
	});

	it("rejects a version that is not in the list", () => {
		assert.throws(() => readBuildTarget({ [VERSIONS_ENV]: versions, [VERSION_ENV]: "v9.9" }));
	});

	it("rejects a malformed list", () => {
		assert.throws(() =>
			readBuildTarget({ [VERSIONS_ENV]: '[{"label":"x","path":"x"}]', [VERSION_ENV]: "x" }),
		);
	});
});

describe("llmsVersionNote", () => {
	const site = "https://docs.example.com";
	const target = (label: string) =>
		readBuildTarget({
			[VERSIONS_ENV]: JSON.stringify(
				planBuilds(["v1.0.0", "v0.9.0"]).map(({ version }) => version),
			),
			[VERSION_ENV]: label,
		});

	it("says nothing for a single version", () => {
		assert.equal(llmsVersionNote(null, site), undefined);
	});

	it("names the version and points at the others", () => {
		assert.equal(
			llmsVersionNote(target("v1.0"), site),
			"This file documents the latest release, v1.0. Other versions have their own (next: https://docs.example.com/next/llms.txt, v0.9: https://docs.example.com/v0.9/llms.txt).",
		);
		assert.match(llmsVersionNote(target("next"), site) ?? "", /unreleased development version/);
		assert.match(
			llmsVersionNote(target("v0.9"), site) ?? "",
			/v0\.9, an older release.*v1\.0: https:\/\/docs\.example\.com\/llms\.txt/,
		);
	});
});
