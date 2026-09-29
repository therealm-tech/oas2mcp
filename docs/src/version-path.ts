/** The same page under another version's path, which may not exist there. */
export function switchVersionPath(pathname: string, from: string, to: string): string {
	return pathname.startsWith(from) ? to + pathname.slice(from.length) : to;
}
