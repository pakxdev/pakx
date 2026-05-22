# examples

Starter templates for both sides of the pakx workflow: **publishing** a
package to the registry, and **consuming** packages from a project.

| Directory | Side | What it shows |
|---|---|---|
| [`hello-world`](./hello-world) | publish | Minimal publishable skill — copy, rename, `pakx publish`. |
| [`agents-yml-starter`](./agents-yml-starter) | consume | A populated `agents.yml` — copy into a project root, then `pakx install`. |

More to come as additional package kinds (`mcp`, `subagent`, `prompt`,
`command`, `hook`) land in the publish flow.
