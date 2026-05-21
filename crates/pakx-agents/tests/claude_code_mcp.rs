//! Integration tests for `ClaudeCodeAdapter::install_mcp` against a temp
//! project root that gets a `.mcp.json`.

use std::collections::BTreeMap;

use pakx_agents::{Adapter, AdapterError, ClaudeCodeAdapter};
use pakx_core::{McpServer, McpTransport};
use serde_json::Value;
use tempfile::TempDir;

fn adapter_for(temp_home: &TempDir, project: &TempDir) -> ClaudeCodeAdapter {
    ClaudeCodeAdapter::with_config_dir(temp_home.path().join(".claude"))
        .with_project_root(project.path())
}

fn stdio_server(id: &str, version: &str) -> McpServer {
    McpServer {
        id: id.into(),
        version: version.into(),
        transport: McpTransport::Stdio {
            command: "npx".into(),
            args: vec!["-y".into(), "@acme/x".into()],
            env: BTreeMap::new(),
        },
    }
}

#[tokio::test]
async fn install_mcp_writes_mcp_json() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let adapter = adapter_for(&home, &project);

    let installed = adapter
        .install_mcp(&stdio_server("io.github.acme/cool", "1.0.0"))
        .await
        .unwrap();
    assert_eq!(installed.id, "io.github.acme/cool");

    let body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    let entry = &v["mcpServers"]["cool"];
    assert_eq!(entry["command"], "npx");
    assert_eq!(entry["args"][1], "@acme/x");
}

#[tokio::test]
async fn install_mcp_preserves_existing_servers() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let adapter = adapter_for(&home, &project);

    // Seed an unrelated server already in `.mcp.json`.
    std::fs::write(
        project.path().join(".mcp.json"),
        r#"{"mcpServers":{"unrelated":{"command":"foo"}}}"#,
    )
    .unwrap();

    adapter
        .install_mcp(&stdio_server("io.github.acme/cool", "1.0.0"))
        .await
        .unwrap();

    let body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(
        v["mcpServers"]["unrelated"].is_object(),
        "unrelated retained"
    );
    assert!(v["mcpServers"]["cool"].is_object(), "new server added");
}

#[tokio::test]
async fn install_mcp_idempotent_returns_already_installed() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let adapter = adapter_for(&home, &project);
    let mcp = stdio_server("a/b", "1.0.0");

    adapter.install_mcp(&mcp).await.unwrap();
    let err = adapter.install_mcp(&mcp).await.unwrap_err();
    assert!(matches!(err, AdapterError::AlreadyInstalled { .. }));
}

#[tokio::test]
async fn install_mcp_rewrites_on_transport_change() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let adapter = adapter_for(&home, &project);

    adapter
        .install_mcp(&stdio_server("a/b", "1.0.0"))
        .await
        .unwrap();

    let mut v2 = stdio_server("a/b", "2.0.0");
    if let McpTransport::Stdio { args, .. } = &mut v2.transport {
        args.push("--new-flag".into());
    }
    adapter.install_mcp(&v2).await.unwrap();

    let body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    assert!(body.contains("--new-flag"), "body=\n{body}");
}

#[tokio::test]
async fn uninstall_mcp_strips_entry() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let adapter = adapter_for(&home, &project);
    adapter
        .install_mcp(&stdio_server("a/cool-server", "1.0.0"))
        .await
        .unwrap();
    adapter.uninstall("a/cool-server").await.unwrap();
    let body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["mcpServers"]["cool-server"].is_null());
}

#[tokio::test]
async fn install_mcp_http_transport_round_trips() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let adapter = adapter_for(&home, &project);
    let mcp = McpServer {
        id: "io.github.acme/web".into(),
        version: "0.1.0".into(),
        transport: McpTransport::Http {
            url: "https://example.com/mcp".into(),
            headers: BTreeMap::new(),
        },
    };
    adapter.install_mcp(&mcp).await.unwrap();
    let body = std::fs::read_to_string(project.path().join(".mcp.json")).unwrap();
    assert!(body.contains("https://example.com/mcp"));
}
