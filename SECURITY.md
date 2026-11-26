# Security policy

## Reporting a vulnerability

Report it through GitHub's
[private vulnerability reporting](https://github.com/therealm-tech/oas2mcp/security/advisories/new).
The report is visible only to you and the maintainers, and the fix is discussed
and prepared there.

Do not open a public issue, pull request or discussion: a vulnerability
described in public is disclosed before it is fixed.

A useful report holds:

- the affected version of the application or the chart;
- the steps to reproduce, with the transport and the options involved;
- the impact as you see it.

Replace any real token, key or credential with a placeholder.

## Supported versions

Fixes ship in a new release. Only the latest release of the application and the
latest release of the Helm chart are supported.

## Scope

A vulnerability in oas2mcp is a flaw in its code, in the container image or in
the Helm chart, for instance:

- bypassing the verification of a caller's JWT, or listing or calling a tool
  the caller's roles do not allow;
- leaking a credential or a token to logs, metrics, MCP clients or an
  unintended server, or serving one caller a token obtained for another;
- crafted input from an MCP client, an upstream response or an OpenAPI document
  that makes oas2mcp send requests the configuration does not allow, crash or
  exhaust its resources;
- a chart default that exposes the workload or grants it more than it needs.

Out of scope:

- a vulnerability in a dependency or in the base image that is already public
  upstream: report it to that project;
- an attack that requires an already compromised host, cluster or account;
- scanner output with no path to exploitation.

## What happens next

Reports are handled on a best-effort basis.

1. The report is acknowledged.
2. It is assessed.
3. The fix is prepared in the advisory's private fork.
4. A new release ships the fix.
5. The advisory is published, with a CVE requested through GitHub when the
   impact warrants one. You are credited unless you prefer otherwise.
