// Pages link each other by root-absolute paths (`/guides/metrics/`), which stay
// the same in every version. This prefixes them with the base the version is
// built under, before the link validator reads them.
import { defineMdastPlugin } from "satteri";

export function prefixBase(url: string, base: string): string {
	if (!url.startsWith("/") || url.startsWith("//") || base === "/") return url;
	return base.replace(/\/$/, "") + url;
}

export function baseLinks(base: string) {
	return defineMdastPlugin({
		name: "base-links",
		link(node, ctx) {
			ctx.setProperty(node, "url", prefixBase(node.url, base));
		},
		definition(node, ctx) {
			ctx.setProperty(node, "url", prefixBase(node.url, base));
		},
	});
}
