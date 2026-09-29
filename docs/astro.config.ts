import { satteri } from "@astrojs/markdown-satteri";
import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";
import starlightLinksValidator from "starlight-links-validator";
import starlightLlmsTxt from "starlight-llms-txt";
import { baseLinks } from "./src/base-links.ts";
import { buildTarget } from "./src/build-target.ts";
import { llmsVersionNote } from "./src/docs-versions.ts";

const base = buildTarget?.current.path ?? "/";
const site = "https://oas2mcp.therealm.tech";
const llmsDetails = llmsVersionNote(buildTarget, site);

export default defineConfig({
	site,
	base,
	srcDir: buildTarget?.srcDir ?? "./src",
	markdown: {
		processor: satteri({ mdastPlugins: [baseLinks(base)] }),
	},
	integrations: [
		starlight({
			title: "oas2mcp",
			description: "Expose every operation of an OpenAPI document as a tool of an MCP server.",
			logo: { src: "../logo.svg" },
			favicon: "/favicon.svg",
			customCss: ["./src/styles/theme.css"],
			components: {
				Banner: "./src/components/Banner.astro",
				SocialIcons: "./src/components/SocialIcons.astro",
			},
			social: [
				{
					icon: "github",
					label: "GitHub",
					href: "https://github.com/therealm-tech/oas2mcp",
				},
			],
			editLink: {
				baseUrl: "https://github.com/therealm-tech/oas2mcp/edit/main/docs/",
			},
			sidebar: [
				{ label: "Getting started", slug: "getting-started" },
				{ label: "Guides", items: [{ autogenerate: { directory: "guides" } }] },
				{ label: "Reference", items: [{ autogenerate: { directory: "reference" } }] },
			],
			plugins: [
				starlightLinksValidator(),
				starlightLlmsTxt(llmsDetails === undefined ? {} : { details: llmsDetails }),
			],
		}),
	],
});
