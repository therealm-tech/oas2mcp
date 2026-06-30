# Xquik OpenAPI Example

This example exposes Xquik REST operations as MCP tools from the hosted
OpenAPI document. Xquik uses the `x-api-key` header for authenticated REST
requests, so pass that header to upstream tool calls.

```bash
export XQUIK_API_KEY="your-api-key"

oas2mcp \
  --openapi-url https://xquik.com/openapi.json \
  --header "x-api-key: ${XQUIK_API_KEY}" \
  --transport streamable-http \
  --bind-addr 127.0.0.1:8000
```

The OpenAPI document is public, but protected API operations still require a
valid API key when the MCP client invokes a tool.
