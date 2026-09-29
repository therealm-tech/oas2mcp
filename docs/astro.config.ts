import starlight from "@astrojs/starlight";
import { defineConfig } from "astro/config";
import starlightLinksValidator from "starlight-links-validator";

export default defineConfig({
	site: "https://oas2mcp.therealm.tech",
	integrations: [
		starlight({
			title: "oas2mcp",
			description: "Expose every operation of an OpenAPI document as a tool of an MCP server.",
			logo: { src: "../logo.svg" },
			favicon: "/favicon.svg",
			customCss: ["./src/styles/theme.css"],
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
			plugins: [starlightLinksValidator()],
		}),
	],
});
