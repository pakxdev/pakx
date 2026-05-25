//! Translate a registry-source [`pakx_registry_client::Package`] into a
//! concrete [`pakx_core::McpTransport`] the adapter can install.
//!
//! v0.1 supports three transport flavours, picked in this priority:
//!   1. Stdio via npm  -> `npx -y <pkg>`
//!   2. Stdio via pypi -> `uvx <pkg>`
//!   3. Stdio via docker / oci -> `docker run -i --rm <image>`
//!   4. Hosted HTTP/SSE -> Http { url, headers }
//!
//! Additional flavours land per-source as registries surface them.

use std::collections::BTreeMap;

use pakx_core::McpTransport;
use pakx_registry_client::Package;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("package {id:?} has no installable transport (no `packages` or `remotes`)")]
    NoTransport { id: String },
    #[error("package {id:?} install_hints schema mismatch: {message}")]
    Schema { id: String, message: String },
}

pub fn translate(pkg: &Package) -> Result<McpTransport, TranslateError> {
    let hints: InstallHints =
        serde_json::from_value(pkg.install_hints.clone()).map_err(|e| TranslateError::Schema {
            id: pkg.id.clone(),
            message: e.to_string(),
        })?;

    if let Some(t) = pick_stdio(&hints, &pkg.id) {
        return Ok(t);
    }
    if let Some(t) = pick_remote(&hints) {
        return Ok(t);
    }
    Err(TranslateError::NoTransport { id: pkg.id.clone() })
}

fn pick_stdio(hints: &InstallHints, _id: &str) -> Option<McpTransport> {
    for pkg in &hints.packages {
        // 2025-12-11 schema moved the transport hint inside each
        // package; SSE / streamable-http packages should resolve to
        // Http, not stdio. Skip those here so pick_remote picks them up.
        if let Some(transport) = &pkg.transport {
            let kind = transport.kind.as_deref().unwrap_or("").to_lowercase();
            if kind == "sse" || kind == "streamable-http" || kind == "http" {
                continue;
            }
        }

        let registry = pkg.registry_name.as_deref().unwrap_or("").to_lowercase();
        let name = pkg.name.as_deref()?;
        let env = collect_env(&pkg.environment_variables);
        let extra_args = collect_positional_args(&pkg.package_arguments);

        let (command, mut args) = match registry.as_str() {
            "npm" | "npmjs" | "npmjs.org" => {
                ("npx".to_owned(), vec!["-y".to_owned(), name.to_owned()])
            }
            "pypi" | "pypi.org" => ("uvx".to_owned(), vec![name.to_owned()]),
            "docker" | "oci" | "ghcr" | "ghcr.io" => (
                "docker".to_owned(),
                vec![
                    "run".to_owned(),
                    "-i".to_owned(),
                    "--rm".to_owned(),
                    name.to_owned(),
                ],
            ),
            _ => continue,
        };
        args.extend(extra_args);
        return Some(McpTransport::Stdio { command, args, env });
    }
    None
}

fn pick_remote(hints: &InstallHints) -> Option<McpTransport> {
    // Legacy `remotes` array — pre-2025-12-11 deployments.
    for r in &hints.remotes {
        if let Some(url) = &r.url {
            return Some(McpTransport::Http {
                url: url.clone(),
                headers: BTreeMap::new(),
            });
        }
    }
    // 2025-12-11 schema embeds the transport inside each package.
    for pkg in &hints.packages {
        if let Some(transport) = &pkg.transport {
            if let Some(url) = &transport.url {
                return Some(McpTransport::Http {
                    url: url.clone(),
                    headers: BTreeMap::new(),
                });
            }
        }
    }
    None
}

fn collect_env(vars: &[EnvVar]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for v in vars {
        if let Some(name) = &v.name {
            // Empty placeholder; user fills in after `pakx install`.
            out.insert(name.clone(), v.default.clone().unwrap_or_default());
        }
    }
    out
}

fn collect_positional_args(args: &[PackageArg]) -> Vec<String> {
    args.iter()
        .filter(|a| a.kind.as_deref() == Some("positional"))
        .filter_map(|a| a.value.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// Wire shape of the official MCP Registry's `extra` JSON.
// Permissive: every field optional; unknown fields ignored.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct InstallHints {
    #[serde(default)]
    packages: Vec<PackageHint>,
    #[serde(default)]
    remotes: Vec<RemoteHint>,
}

#[derive(Debug, Deserialize)]
struct PackageHint {
    // Pre-2025-12-11 schema used `registry_name`; the new schema uses
    // `registryType` (e.g. "npm"). Accept both.
    #[serde(default, alias = "registry_name", alias = "registryType")]
    registry_name: Option<String>,
    // Pre-2025-12-11 used `name`; new schema uses `identifier`.
    #[serde(default, alias = "name", alias = "identifier")]
    name: Option<String>,
    #[serde(default, alias = "package_arguments", alias = "packageArguments")]
    package_arguments: Vec<PackageArg>,
    #[serde(
        default,
        alias = "environment_variables",
        alias = "environmentVariables"
    )]
    environment_variables: Vec<EnvVar>,
    #[serde(default)]
    transport: Option<TransportHint>,
}

#[derive(Debug, Deserialize)]
struct TransportHint {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackageArg {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EnvVar {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemoteHint {
    #[serde(default, alias = "transport_type", alias = "type")]
    #[allow(dead_code)]
    transport_type: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pakx_core::RegistrySource;
    use serde_json::json;

    fn pkg(hints: serde_json::Value) -> Package {
        Package {
            id: "io.github.acme/cool".into(),
            source: RegistrySource::OfficialMcp,
            name: "io.github.acme/cool".into(),
            version: "1.0.0".into(),
            description: None,
            kind: None,
            install_hints: hints,
        }
    }

    #[test]
    fn npm_package_translates_to_npx_stdio() {
        let p = pkg(json!({
            "packages": [
                { "registry_name": "npm", "name": "@acme/cool-mcp" }
            ]
        }));
        let McpTransport::Stdio { command, args, .. } = translate(&p).unwrap() else {
            panic!("expected stdio");
        };
        assert_eq!(command, "npx");
        assert_eq!(args, vec!["-y", "@acme/cool-mcp"]);
    }

    #[test]
    fn pypi_package_translates_to_uvx_stdio() {
        let p = pkg(json!({
            "packages": [
                { "registry_name": "pypi", "name": "acme-cool-mcp" }
            ]
        }));
        let McpTransport::Stdio { command, args, .. } = translate(&p).unwrap() else {
            panic!("expected stdio");
        };
        assert_eq!(command, "uvx");
        assert_eq!(args, vec!["acme-cool-mcp"]);
    }

    #[test]
    fn env_vars_become_empty_placeholders() {
        let p = pkg(json!({
            "packages": [
                {
                    "registry_name": "npm",
                    "name": "@acme/x",
                    "environment_variables": [
                        { "name": "API_KEY" },
                        { "name": "REGION", "default": "us-east-1" }
                    ]
                }
            ]
        }));
        let McpTransport::Stdio { env, .. } = translate(&p).unwrap() else {
            panic!("expected stdio");
        };
        assert_eq!(env.get("API_KEY"), Some(&String::new()));
        assert_eq!(env.get("REGION"), Some(&"us-east-1".to_owned()));
    }

    #[test]
    fn remote_falls_back_to_http() {
        let p = pkg(json!({
            "remotes": [
                { "transport_type": "sse", "url": "https://x.example/mcp" }
            ]
        }));
        let McpTransport::Http { url, .. } = translate(&p).unwrap() else {
            panic!("expected http");
        };
        assert_eq!(url, "https://x.example/mcp");
    }

    #[test]
    fn no_transport_errors() {
        let p = pkg(json!({}));
        assert!(matches!(
            translate(&p),
            Err(TranslateError::NoTransport { .. })
        ));
    }

    #[test]
    fn new_schema_field_names_decode_via_serde_aliases() {
        // 2025-12-11 MCP Registry schema: registryType / identifier /
        // packageArguments / environmentVariables.
        let p = pkg(json!({
            "packages": [{
                "registryType": "npm",
                "identifier": "@acme/cool-mcp",
                "packageArguments": [
                    { "type": "positional", "value": "--port=3000" }
                ],
                "environmentVariables": [
                    { "name": "API_KEY" }
                ]
            }]
        }));
        let McpTransport::Stdio { command, args, env } = translate(&p).unwrap() else {
            panic!("expected stdio");
        };
        assert_eq!(command, "npx");
        assert_eq!(args, vec!["-y", "@acme/cool-mcp", "--port=3000"]);
        assert_eq!(env.get("API_KEY"), Some(&String::new()));
    }

    #[test]
    fn embedded_transport_url_resolves_to_http() {
        // 2025-12-11 schema can embed an SSE / streamable-http transport
        // directly inside a package entry. pick_remote walks `packages[]`
        // when `remotes[]` is absent.
        let p = pkg(json!({
            "packages": [{
                "registryType": "npm",
                "identifier": "@acme/hosted-mcp",
                "transport": {
                    "type": "streamable-http",
                    "url": "https://hosted.example/mcp"
                }
            }]
        }));
        let McpTransport::Http { url, .. } = translate(&p).unwrap() else {
            panic!("expected http transport");
        };
        assert_eq!(url, "https://hosted.example/mcp");
    }

    #[test]
    fn positional_args_appended() {
        let p = pkg(json!({
            "packages": [{
                "registry_name": "npm",
                "name": "@acme/x",
                "package_arguments": [
                    { "type": "positional", "value": "--foo" },
                    { "type": "named", "value": "ignored" }
                ]
            }]
        }));
        let McpTransport::Stdio { args, .. } = translate(&p).unwrap() else {
            panic!("expected stdio");
        };
        assert_eq!(args, vec!["-y", "@acme/x", "--foo"]);
    }
}
