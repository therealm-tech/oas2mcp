import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { markdownToHtml } from "satteri";
import { baseLinks, prefixBase } from "./base-links.ts";

describe("prefixBase", () => {
	it("prefixes a root-absolute link", () => {
		assert.equal(prefixBase("/guides/metrics/#outcome", "/next/"), "/next/guides/metrics/#outcome");
	});

	it("leaves everything else alone", () => {
		for (const url of ["https://example.com/", "//cdn.example.com/x", "#anchor", "../metrics/"]) {
			assert.equal(prefixBase(url, "/v0.9/"), url);
		}
	});

	it("is a no-op at the root", () => {
		assert.equal(prefixBase("/guides/metrics/", "/"), "/guides/metrics/");
	});
});

describe("baseLinks", () => {
	it("rewrites inline and reference links in rendered Markdown", () => {
		const { html } = markdownToHtml(
			"- see [tools](/reference/tools/)\n- and [the config][cfg]\n\n[cfg]: /reference/configuration/\n",
			{ mdastPlugins: [baseLinks("/next/")] },
		);
		assert.match(html, /href="\/next\/reference\/tools\/"/);
		assert.match(html, /href="\/next\/reference\/configuration\/"/);
	});
});
