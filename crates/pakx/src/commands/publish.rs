//! `pakx publish [<path>]` — pack + upload to the pakx-registry.
//!
//! Two-step API contract:
//!   1. POST /api/v1/packages              { name, kind, description? }
//!      -> upserts the package row (owner is taken from the bearer token).
//!   2. PUT  /api/v1/packages/<owner>/<name>/<version>
//!      -> uploads the tarball bytes. Returns sha256 + signed URL.
//!
//! Output modes:
//!
//! - **Human (default).** Progress + warnings stream to stderr with the
//!   project's `[ok]` / `[warn]` glyph cadence; stdout stays silent.
//! - **`--json`.** Progress + warnings still go to stderr so CI logs
//!   keep the warning trail; stdout receives a **single**
//!   newline-terminated JSON object once the upload completes. Field
//!   names are a stable camelCase contract — `jq` consumers can pipe
//!   directly.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use pakx_core::{Credentials, DEFAULT_REGISTRY_URL};
use pakx_registry_client::{BackendError, CreatePackageRequest, PakxBackend};

use crate::pack::pack_dir;
use crate::registry_url::validate_base_url;
use crate::ui;

/// Closed set of package kinds the registry accepts. Constraining the
/// flag at clap-parse time means a typo (`--kind banan`) fails *before*
/// we pack the bundle + upload it — previously the wasted work
/// surfaced only as a registry-side 400 after the tarball round-trip.
/// Variant order + names mirror `pakx_core::manifest::PackageType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum PublishKind {
    Skills,
    Mcp,
    Subagents,
    Prompts,
    Commands,
    Hooks,
}

impl PublishKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Subagents => "subagents",
            Self::Prompts => "prompts",
            Self::Commands => "commands",
            Self::Hooks => "hooks",
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct PublishArgs {
    /// Source directory. Defaults to cwd.
    pub source: Option<PathBuf>,

    /// Registry. Defaults to <https://registry.pakx.dev>.
    #[arg(short = 'r', long = "registry", default_value = DEFAULT_REGISTRY_URL)]
    pub registry: String,

    /// Package kind. Defaults to "skills" (the v0 use-case).
    ///
    /// Constrained to the six registry-known kinds (skills, mcp,
    /// subagents, prompts, commands, hooks) so a typo fails at flag
    /// parse — *before* we pack + upload — instead of bubbling up as
    /// a registry-side 400 after the tarball round-trip.
    #[arg(
        short = 'k',
        long = "kind",
        value_enum,
        default_value_t = PublishKind::Skills,
    )]
    pub kind: PublishKind,

    /// Optional one-line description.
    #[arg(short = 'd', long = "description")]
    pub description: Option<String>,

    /// Print what would happen but don't actually upload.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit a single machine-readable JSON object on stdout describing
    /// the publish outcome. Progress lines + warnings still go to
    /// stderr. Field names are a stable contract for downstream
    /// pipelines (`registryUrl`, `tarballUrl`, `sha256`, ...).
    #[arg(long)]
    pub json: bool,

    #[arg(long, hide = true)]
    pub credentials_file: Option<PathBuf>,
}

#[allow(clippy::too_many_lines)] // linear flow; helpers would obscure shape
pub async fn run(args: PublishArgs) -> Result<()> {
    // Vet any user-supplied `--registry` BEFORE the credentials lookup
    // or any HTTP work. The publish flow sends the bearer token + the
    // tarball bytes; a userinfo-smuggled override would exfiltrate
    // both. Mirrors `pakx login` / `pakx install` discipline — the
    // single source of truth for the validator is
    // `crate::registry_url::validate_base_url`.
    if args.registry != DEFAULT_REGISTRY_URL {
        validate_base_url(&args.registry)?;
    }
    if args.json {
        // Keep stdout byte-clean for `--json | jq`. Spinners + progress
        // lines still color on stderr — only the machine-readable
        // payload route on stdout is flattened.
        ui::force_stdout_no_color();
    }
    let src = args.source.clone().unwrap_or_else(|| PathBuf::from("."));
    let creds_path = match args.credentials_file.clone() {
        Some(p) => p,
        None => Credentials::default_path().context("resolve credentials path")?,
    };
    let creds = Credentials::read_from(&creds_path).context("read credentials")?;
    let entry = creds
        .get(&args.registry)
        .ok_or_else(|| anyhow::anyhow!("not logged in to {} — run `pakx login`", args.registry))?;

    let pb = ui::spinner("packing");
    // Pack into a `TempDir` guard rather than the bare system temp dir.
    // `pack_dir` writes a `<name>-<version>.tgz` file into `out_dir`;
    // packing straight into `std::env::temp_dir()` leaked that tarball
    // into the user's temp forever (one orphaned `.tgz` per publish).
    // The guard's `Drop` removes the directory (and the tarball inside)
    // once we return — mirrors the round-39 cache-tempdir pattern. We
    // keep `pack_tmp` bound through the upload so the directory survives
    // until the function exits; `pack.bytes` already holds the archive
    // contents in memory, so the on-disk file is only ever transient.
    let pack_tmp = tempfile::TempDir::new().context("create pack temp dir")?;
    let pack = pack_dir(&src, pack_tmp.path())?;
    pb.finish_and_clear();
    // Warnings stream to stderr regardless of `--json` so CI logs always
    // surface the publisher hygiene hints (missing `description:`, etc.)
    // — the JSON payload also carries them so a `--json | jq .warnings`
    // pipeline doesn't need a separate stderr capture.
    for warning in &pack.warnings {
        eprintln!("{} {warning}", ui::glyph_warn_err());
    }
    eprintln!(
        "{} packed {} ({} bytes)",
        ui::glyph_ok_err(),
        ui::success_err(&format!("{}@{}", pack.manifest.name, pack.manifest.version)),
        pack.bytes.len()
    );

    if args.dry_run {
        // `--dry-run` short-circuits before the registry round-trip.
        // The JSON contract still applies: emit a stub object that
        // tooling can detect via `"ok": true` + `"dryRun": true` and
        // skip the assertion on `registryUrl` / `tarballUrl` /
        // `publishedAt`. Human mode keeps the v0 stderr hint.
        if args.json {
            let payload = serde_json::json!({
                "ok": true,
                "dryRun": true,
                "name": pack.manifest.name,
                "version": pack.manifest.version,
                "sizeBytes": pack.bytes.len(),
                "warnings": pack.warnings,
            });
            let line = serde_json::to_string(&payload).expect("serialize publish dry-run json");
            println!("{line}");
        } else {
            eprintln!("{}", ui::dim_err("dry-run: skipping upload"));
        }
        return Ok(());
    }

    let backend = PakxBackend::new(&args.registry);
    let pb = ui::spinner("creating package row");
    // Spec §2 / parent prompt §Publish-emit: omit `sponsors` from the
    // POST body when the manifest declares none. The registry treats an
    // absent field as "no change" but an explicit `[]` as "clear", so
    // omitting on empty avoids wiping sponsors on a republish from an
    // older manifest that hasn't been re-edited.
    let sponsors_payload =
        (!pack.manifest.sponsors.is_empty()).then_some(pack.manifest.sponsors.as_slice());
    // README captured at pack-time from the bundle's `README.md`.
    // Forwarded on publish when present so the registry's omit-vs-
    // explicit semantics fire — a bundle without a README on republish
    // never wipes a previously-stored README. `readme` is `None` for
    // bundles that ship no README; we forward as `None`, which the
    // serializer skips entirely (see `CreatePackageRequest::readme`).
    let readme_payload = pack.manifest.readme.as_deref();
    let pkg = backend
        .create_package(
            &entry.token,
            &CreatePackageRequest {
                name: &pack.manifest.name,
                kind: args.kind.as_str(),
                description: args.description.as_deref(),
                sponsors: sponsors_payload,
                readme: readme_payload,
            },
        )
        .await
        .map_err(|e| handle_backend_err(&e, args.json))?;
    pb.finish_and_clear();
    eprintln!(
        "{} {} {} on {}",
        ui::glyph_ok_err(),
        if pkg.created { "created" } else { "reusing" },
        pkg.id,
        args.registry
    );

    let pb = ui::spinner("uploading tarball");
    let upload = backend
        .upload_version(
            &entry.token,
            &pkg.owner,
            &pkg.name,
            &pack.manifest.version,
            pack.bytes,
            readme_payload,
        )
        .await
        .map_err(|e| handle_backend_err(&e, args.json))?;
    pb.finish_and_clear();
    // Truncate the sha256 for the human line via `get(..16)`, NOT a raw
    // byte-slice. The hash is a registry-returned string; a short or
    // multibyte value would make `&upload.sha256[..16]` panic on a byte
    // index that doesn't land on a char boundary — AFTER a successful
    // upload, so the user would see a publish that "failed" despite the
    // package being live. Fall back to the full string when it's shorter
    // than 16 bytes.
    let sha_display = upload.sha256.get(..16).unwrap_or(&upload.sha256);
    eprintln!(
        "{} uploaded {} v{} ({} bytes, sha256 {})",
        ui::glyph_ok_err(),
        ui::success_err(&upload.id),
        upload.version,
        upload.size_bytes,
        sha_display,
    );
    eprintln!(
        "{}",
        ui::success_err(&format!(
            "published {}/{}@{}",
            pkg.owner, pkg.name, upload.version
        ))
    );

    if args.json {
        // Dashboard route — same shape as the human "→ view:" hint
        // below. Anchored on `https://pakx.dev` (the public dashboard)
        // independent of `args.registry`, which is the **API** base.
        let registry_url = format!(
            "https://pakx.dev/p/pakx/{}/{}/{}",
            pkg.owner, pkg.name, upload.version
        );
        let payload = serde_json::json!({
            "ok": true,
            "name": format!("{}/{}", pkg.owner, pkg.name),
            "version": upload.version,
            "sha256": upload.sha256,
            "sizeBytes": upload.size_bytes,
            "registryUrl": registry_url,
            "tarballUrl": upload.tarball_url,
            // `publishedAt` is part of the per-version detail endpoint
            // (see `pakx info --version`) but not the upload response,
            // so we emit `null` to keep the shape forward-compatible
            // — a future backend field would land here without
            // breaking jq pipelines that already key off it.
            "publishedAt": serde_json::Value::Null,
            "warnings": pack.warnings,
        });
        let line = serde_json::to_string(&payload).expect("serialize publish json");
        println!("{line}");
        return Ok(());
    }

    // Single dimmed next-step hint pointing at the public dashboard
    // listing. The URL shape `https://pakx.dev/p/pakx/<owner>/<name>`
    // matches the dashboard route — the trailing `pakx` segment is
    // the source tag, mirroring the federated-source key used in
    // `agents.lock`.
    eprintln!(
        "{}",
        ui::dim_err(&format!(
            "\u{2192} view: https://pakx.dev/p/pakx/{}/{}",
            pkg.owner, pkg.name
        ))
    );
    Ok(())
}

/// Convert a `BackendError` into a multi-line `anyhow::Error` whose
/// `Display` is the publisher-facing hint shown on the terminal's last
/// red line. When `json_mode` is set we ALSO emit a single-line JSON
/// error envelope on stdout BEFORE returning the typed `anyhow` — that
/// way `pakx publish --json | jq` consumers get a structured
/// `{errorKind, fixHint, upstreamCode}` block instead of being forced
/// to parse the human-readable stderr hint.
///
/// Mapping is grounded in `pakx-registry`'s actual emit sites — every
/// arm is matched against a code the registry is documented to send
/// from the POST upsert or the PUT version handler. Codes the registry
/// doesn't emit are NOT mapped (so a stray future status doesn't show
/// up as a dead branch); the unknown-status fallthrough preserves the
/// upstream message so a wire-shape drift surfaces verbatim instead of
/// being smothered by a generic hint.
fn handle_backend_err(e: &BackendError, json_mode: bool) -> anyhow::Error {
    if json_mode {
        emit_publish_error_json(e);
    }
    map_backend_err(e)
}

fn map_backend_err(e: &BackendError) -> anyhow::Error {
    match e {
        BackendError::Unauthorized => anyhow::anyhow!(
            "Token expired or invalid.\n  \
             Fix: run `pakx login` to re-authenticate.\n  \
             Tokens last 90 days from issue; check `pakx whoami` for the active login.",
        ),
        BackendError::Forbidden => anyhow::anyhow!(
            "You don't own this package, or it's already taken under a different account.\n  \
             Fix: run `pakx whoami` to confirm your logged-in account, then pick a different `name:` in the manifest.\n  \
             Names are owned per-publisher — there's no rename path once a name is registered.",
        ),
        BackendError::NotFound => anyhow::anyhow!(
            "Package not found on registry.\n  \
             Fix: run `pakx publish` from the bundle root so the upsert step (POST /api/v1/packages) registers the name before the version upload.",
        ),
        // Round-77 verify-before-cite: 411 surfaces when a proxy strips
        // Content-Length or the CLI got swapped for a chunked-encoding
        // client. The pakx CLI itself always sends a declared length,
        // so the publisher-facing fix points at the toolchain in
        // between, not at the bundle contents.
        BackendError::LengthRequired => anyhow::anyhow!(
            "Registry rejected the upload without a Content-Length header (411).\n  \
             Fix: the pakx CLI always sends one — a 411 here points at a corporate proxy or VPN stripping the header.\n  \
             Workaround: retry from a different network, or set `--registry` to a self-hosted instance bypassing the proxy.",
        ),
        // 413 carries the registry-side cap (`maxBytes`) in the JSON
        // body when available. The current cap is 50 MiB
        // (`TARBALL_MAX_BYTES` in the registry's version PUT handler).
        BackendError::TooLarge { max_bytes } => {
            let cap = max_bytes.map_or_else(
                || "the registry's hard cap".to_owned(),
                |n| format!("{} MiB", n / (1024 * 1024)),
            );
            anyhow::anyhow!(
                "Tarball too large (413). Max size: {cap}.\n  \
                 Fix: prune large binaries / generated assets / `node_modules` from the bundle and re-publish.\n  \
                 Tip: `pakx pack` prints the byte size — run it locally to size-check before publishing.",
            )
        }
        // 409 on the POST upsert path. The registry's response body
        // carries the stored + received kinds so the hint can quote
        // both sides. A kind change isn't fixable in-place (kind picks
        // the install destination dir).
        BackendError::KindMismatch { stored, received } => {
            let stored_str = stored.as_deref().unwrap_or("<unknown>");
            let received_str = received.as_deref().unwrap_or("<unknown>");
            anyhow::anyhow!(
                "Package kind conflict (409): the registry has this name registered as `{stored_str}`, but you published with `--kind {received_str}`.\n  \
                 Fix: a package's kind is immutable. Publish under a new `name:` in the manifest, or run `pakx publish --kind {stored_str}` to match.",
            )
        }
        // 409 on the PUT version path. The fix is always "bump the
        // version in agents.yml" — re-uploads of an existing version
        // are refused by design.
        BackendError::VersionExists => anyhow::anyhow!(
            "This version was already published (409).\n  \
             Fix: bump `version:` in the manifest (or `agents.yml`) and re-publish.\n  \
             Versions are immutable once accepted — re-uploading the same `version:` is refused on purpose.",
        ),
        // Legacy `Conflict` variant from the prior wire shape. Kept so
        // any caller that constructs a `Conflict` directly (tests,
        // future code paths) still gets a sensible hint.
        BackendError::Conflict { message } => anyhow::anyhow!(
            "Conflict from registry: {message}\n  \
             Fix: re-run with a bumped `version:` if this is a re-publish, or `pakx whoami` to confirm the right account.",
        ),
        // 400 with optional `detail`. The registry emits `detail` for
        // zod-refusal (POST schema) and named cases (`empty`,
        // oversize README). When present we echo it verbatim — the
        // registry already wrote publisher-friendly copy.
        BackendError::Invalid { detail } => {
            let detail_str = detail
                .as_deref()
                .unwrap_or("see https://pakx.dev/docs/manifest for the manifest schema");
            anyhow::anyhow!(
                "Registry refused the request (400): {detail_str}\n  \
                 Fix: correct the manifest field flagged above and re-publish.\n  \
                 Schema reference: https://pakx.dev/docs/manifest",
            )
        }
        // 429 carries `Retry-After` (seconds) when the limiter trips.
        // The publish bucket is per-user (20 burst, ~6/min sustained);
        // a publisher who actually tripped it almost certainly did so
        // by accident.
        BackendError::RateLimited { retry_after_secs } => {
            let wait = retry_after_secs.unwrap_or(60);
            anyhow::anyhow!(
                "Rate limited (429). Retry after: {wait}s.\n  \
                 Fix: wait the indicated interval, then re-run `pakx publish`.\n  \
                 The publish bucket is per-user (20 burst, ~6/min sustained); if this persists, check for a script re-publishing in a loop.",
            )
        }
        // 500 — the registry's `internalError()` helper has redacted
        // the body in production. Best we can offer is "report it".
        BackendError::Internal => anyhow::anyhow!(
            "Registry internal error (500).\n  \
             Fix: retry in a minute. If it persists, file an issue at https://github.com/pakxdev/pakx-registry/issues with the manifest name + version.",
        ),
        // Validation lifts from `pakx-core` — these mean the CLI
        // refused to dial out at all (hostile path segment in
        // `name` / `owner` / `version`). Hint says fix the manifest.
        BackendError::InvalidName { name, reason } => anyhow::anyhow!(
            "Invalid package name `{name}`: {reason}.\n  \
             Fix: edit `name:` in the manifest — kebab-case (`my-skill`), no `..`, no `/`.",
        ),
        BackendError::InvalidVersion { version, reason } => anyhow::anyhow!(
            "Invalid version `{version}`: {reason}.\n  \
             Fix: use a SemVer-compatible value (`0.1.0`, `1.0.0-rc.1`).",
        ),
        // Transport-layer reqwest error. Most often a DNS miss / TLS
        // failure / proxy timeout — surface verbatim so the operator
        // sees the underlying message.
        BackendError::Http(http_err) => anyhow::anyhow!(
            "Network error talking to registry: {http_err}\n  \
             Fix: check connectivity, then re-run `pakx publish`.\n  \
             If you're behind a proxy, set `HTTPS_PROXY` and retry.",
        ),
        // Unknown status — surface the upstream body verbatim so a
        // future code never gets smothered by a generic hint.
        BackendError::Other { status, body } => anyhow::anyhow!(
            "Registry error ({status}): {body}\n  \
             This status isn't mapped to a specific fix-hint — file an issue at https://github.com/pakxdev/pakx/issues so we can add one.",
        ),
    }
}

/// Emit a single-line JSON envelope on stdout describing the publish
/// failure. Additive contract to the round-32 `pakx publish --json`
/// shape: every payload carries `ok: false`, a CLI-stable `errorKind`
/// discriminator, the canonical `upstreamCode` from the registry, a
/// one-sentence `fixHint`, and (for the codes that emit them) any
/// structured extras the registry returned (`retryAfterSeconds`,
/// `maxBytes`, `detail`, `stored`/`received` kind).
///
/// `errorKind` strings are part of the `pakx publish --json` contract —
/// downstream jq pipelines may branch on them. They are CHOSEN to be
/// stable across registry minor versions even if the registry-side
/// canonical name changes.
#[allow(clippy::too_many_lines)] // 13-arm match; splitting per-variant would only obscure shape
fn emit_publish_error_json(e: &BackendError) {
    let payload = match e {
        BackendError::Unauthorized => serde_json::json!({
            "ok": false,
            "errorKind": "unauthorized",
            "upstreamCode": 401,
            "fixHint": "Run `pakx login` to re-authenticate.",
        }),
        BackendError::Forbidden => serde_json::json!({
            "ok": false,
            "errorKind": "forbidden",
            "upstreamCode": 403,
            "fixHint": "Pick a different `name:` in the manifest — this one is owned by another account.",
        }),
        BackendError::NotFound => serde_json::json!({
            "ok": false,
            "errorKind": "not-found",
            "upstreamCode": 404,
            "fixHint": "Re-run `pakx publish` from the bundle root so the upsert step registers the name before the version upload.",
        }),
        BackendError::LengthRequired => serde_json::json!({
            "ok": false,
            "errorKind": "length-required",
            "upstreamCode": 411,
            "fixHint": "A proxy stripped Content-Length. Retry from a different network.",
        }),
        BackendError::TooLarge { max_bytes } => serde_json::json!({
            "ok": false,
            "errorKind": "tarball-too-large",
            "upstreamCode": 413,
            "maxBytes": max_bytes,
            "fixHint": "Prune large files from the bundle and re-publish.",
        }),
        BackendError::KindMismatch { stored, received } => serde_json::json!({
            "ok": false,
            "errorKind": "kind-mismatch",
            "upstreamCode": 409,
            "stored": stored,
            "received": received,
            "fixHint": "Publish under a new `name:`, or pass the stored kind via `--kind`.",
        }),
        BackendError::VersionExists => serde_json::json!({
            "ok": false,
            "errorKind": "version-exists",
            "upstreamCode": 409,
            "fixHint": "Bump `version:` in the manifest and re-publish.",
        }),
        BackendError::Conflict { message } => serde_json::json!({
            "ok": false,
            "errorKind": "conflict",
            "upstreamCode": 409,
            "detail": message,
            "fixHint": "Re-run with a bumped `version:`, or confirm the right account via `pakx whoami`.",
        }),
        BackendError::Invalid { detail } => serde_json::json!({
            "ok": false,
            "errorKind": "invalid-request",
            "upstreamCode": 400,
            "detail": detail,
            "fixHint": "Correct the manifest field flagged in `detail` and re-publish.",
        }),
        BackendError::RateLimited { retry_after_secs } => serde_json::json!({
            "ok": false,
            "errorKind": "rate-limited",
            "upstreamCode": 429,
            "retryAfterSeconds": retry_after_secs,
            "fixHint": "Wait the indicated interval and re-run `pakx publish`.",
        }),
        BackendError::Internal => serde_json::json!({
            "ok": false,
            "errorKind": "registry-internal",
            "upstreamCode": 500,
            "fixHint": "Retry in a minute. If persistent, file an issue.",
        }),
        BackendError::InvalidName { name, reason } => serde_json::json!({
            "ok": false,
            "errorKind": "invalid-name",
            "name": name,
            "reason": reason,
            "fixHint": "Edit `name:` in the manifest — kebab-case, no `..`, no `/`.",
        }),
        BackendError::InvalidVersion { version, reason } => serde_json::json!({
            "ok": false,
            "errorKind": "invalid-version",
            "version": version,
            "reason": reason,
            "fixHint": "Use a SemVer-compatible value (`0.1.0`, `1.0.0-rc.1`).",
        }),
        BackendError::Http(http_err) => serde_json::json!({
            "ok": false,
            "errorKind": "network",
            "detail": http_err.to_string(),
            "fixHint": "Check connectivity, then re-run `pakx publish`.",
        }),
        BackendError::Other { status, body } => serde_json::json!({
            "ok": false,
            "errorKind": "unmapped",
            "upstreamCode": status,
            "detail": body,
            "fixHint": "File an issue at https://github.com/pakxdev/pakx/issues so we can add a hint.",
        }),
    };
    let line = serde_json::to_string(&payload).expect("serialize publish error json");
    println!("{line}");
}
