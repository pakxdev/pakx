# agents-yml-starter

A consumer-side `agents.yml` template — the manifest a project keeps in
its repo root to declare which skills, MCP servers, and other agent
packages it depends on. Copy [`agents.yml`](./agents.yml) into a new
project, trim the dep lists to what you actually want installed, run
`pakx install`, and point your agent (Claude Code, Cursor, Codex,
Copilot, Windsurf) at the project root. The manifest is the install-side
counterpart to the publish-side template in [`../hello-world`](../hello-world).
