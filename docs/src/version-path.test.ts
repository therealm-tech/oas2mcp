import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { switchVersionPath } from "./version-path.ts";

describe("switchVersionPath", () => {
	it("keeps the page when switching versions", () => {
		assert.equal(switchVersionPath("/next/guides/metrics/", "/next/", "/"), "/guides/metrics/");
		assert.equal(switchVersionPath("/guides/metrics/", "/", "/v0.9/"), "/v0.9/guides/metrics/");
	});

	it("falls back to the target's home outside the current version", () => {
		assert.equal(switchVersionPath("/elsewhere/", "/next/", "/v0.9/"), "/v0.9/");
	});
});
