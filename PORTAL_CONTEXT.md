# Portal Context

This file is the canonical orientation document for the Portal repository.

Before making architectural changes, read:

1. `PORTAL_CONTEXT.md`
2. `AGENTS.md`
3. the relevant domain documentation under `docs/`
4. the actual source files being modified

If documentation and source code disagree, source code is authoritative and
`PORTAL_CONTEXT.md` must be corrected in the same change.

## Product purpose

Portal is a native desktop workspace and shell built with Rust, Slint, and
local-first storage. It hosts explicitly compiled internal applications. It is
not a browser shell, an arbitrary command runner, or a dynamic plugin host.

The current application areas are Home, Nexus, Compute, and Settings:

- Home provides the existing filesystem-backed Project -> Shot workflow.
- Nexus is an AI Content Studio: a chat-first content-generation workspace for
  conversations, characters, reference media, generation jobs, and generated
  media.
- Compute presents Nexus's RunPod setup, infrastructure, and job history.
- Settings presents Nexus compute credentials, preflight diagnostics, manual
  endpoint fallbacks, prompt-compiler configuration, and local storage tools.

Nexus has an implemented RunPod and FLUX.2 TextToImage path. Image-to-image and
video modes are represented in domain and UI models but do not have production
workers in this repository. Chat messages persist locally; no production local
conversational LLM or prompt compiler is implemented.

## Current version and status

Portal application version: **0.8.0**

Current FLUX worker image: **`ghcr.io/jakob-plappert/portal-comfy-worker:0.6.0`**

Status terms in this document are deliberate:

- **IMPLEMENTED**: source and automated tests contain the capability.
- **PARTIAL**: a useful portion exists, but the named capability is incomplete.
- **PLANNED**: modeled or discussed, but not operational.
- **LIVE-VERIFIED**: a human has exercised the real external path and the
  repository contains an explicit record supporting that claim.

| Area | Status | Notes |
| --- | --- | --- |
| Portal shell and navigation | IMPLEMENTED | Native Slint window with a Rust-owned compile-time app catalog for Home, Nexus, Compute, and Settings. |
| Home Project -> Shot workflow | IMPLEMENTED | TOML and ordinary directories under `portaldata/projects/`. |
| Nexus local persistence | IMPLEMENTED | App-scoped SQLite, settings, infrastructure identity, and ordinary media files. |
| Conversations and messages | IMPLEMENTED | Local SQLite persistence; chat does not invent assistant replies. |
| Character database and references | IMPLEMENTED | Character CRUD subset plus image reference associations. |
| Media library | IMPLEMENTED | Local import, metadata, thumbnails/previews, and generated-image display. |
| Generation jobs | IMPLEMENTED | Durable states, remote IDs, metrics, failure details, and restart reconciliation. |
| RunPod/GHCR/local preflight | IMPLEMENTED | Read-only diagnostics, anonymous OCI Bearer flow, discovery, permission reads, and storage probes. |
| RunPod infrastructure planning/provisioning | IMPLEMENTED | Discover -> Plan -> Confirm -> Apply for a Network Volume and managed FLUX endpoint. |
| FLUX.2 TextToImage worker | IMPLEMENTED | Fixed ComfyUI API workflow and Network Volume model bootstrap. |
| Real end-to-end FLUX generation | READY FOR LIVE TEST / NOT YET VERIFIED | Code and tests are present, but the repository records no completed live RunPod image with locally rendered PNG. |
| ImageToImage | PARTIAL | Mode and FLUX catalog capability are modeled; the deployed worker explicitly rejects it. |
| LTX video | PLANNED | Catalog/domain entries only; no LTX worker or workflow. |
| Wan video | PLANNED | Catalog/domain entries only; no Wan worker or workflow. |
| Local conversational LLM | PLANNED | Contract direction and a minimal llama.cpp container placeholder exist; no production integration or model download. |
| Production prompt compiler | PLANNED | Serializable request/result types and settings exist; no compiler execution path is implemented. |

## Repository map

| Path | Ownership |
| --- | --- |
| `portal-app/` | The authoritative desktop application: UI, internal app catalog, local state, Project -> Shot data, Nexus SQLite, characters, media, jobs, RunPod orchestration, confirmations, and provider resource IDs. |
| `portal-comfy-worker/` | A narrow remote compute adapter. It validates `MediaWorkerRequest`, maps supported input into a fixed ComfyUI workflow, and returns `MediaWorkerResponse`. It does not own Portal domain state. |
| `portal-llm-worker/` | The future prompt/scene compiler direction. It currently contains only a minimal llama.cpp server container definition and is not Portal's source of truth. |
| `docs/` | Domain and contributor documentation. `runpod-nexus.md` is the detailed RunPod operational guide. |
| `.github/` | GitHub Actions, currently including build/publish automation for the Comfy worker image. |
| `portaldata/` | Local runtime data, databases, projects, caches, and media. It is ignored by Git and must never be committed. |

## Internal apps

`portal-app/src/app_catalog.rs` is the authoritative Rust inventory. The apps
are concrete, internal, and compiled with Portal. Adding one requires a source
change, tests, and review. Portal intentionally does not dynamically load
arbitrary plugins, manifests, shared libraries, scripts, or external UI code.

| ID | Display name | Purpose | Storage ownership | Current status | Important entry points |
| --- | --- | --- | --- | --- | --- |
| `home` | Home | Project and Shot creation/opening | Existing shell-level `portaldata/projects/`; no `apps/home/` directory | IMPLEMENTED | `app_catalog.rs`, `main.rs`, `project.rs`, `shot.rs`, `ui/main.slint` |
| `nexus` | Nexus | AI Content Studio and generation workspace | `portaldata/apps/nexus/` | IMPLEMENTED, with deferred backends | `app_state.rs`, `nexus_ui.rs`, `storage.rs`, `generation.rs`, `ui/main.slint` |
| `compute` | Compute | View and control Nexus RunPod setup and jobs | Reads Nexus-owned state; no `apps/compute/` directory | IMPLEMENTED | `nexus_ui.rs`, `setup.rs`, `provisioning.rs`, `runpod.rs`, `ui/main.slint` |
| `settings` | Settings | Configure Nexus compute and inspect local storage/diagnostics | Nexus settings plus OS keyring; no `apps/settings/` directory | IMPLEMENTED | `settings.rs`, `setup.rs`, `platform.rs`, `nexus_ui.rs`, `ui/main.slint` |

The catalog supplies stable IDs, display metadata, optional storage IDs, and
the Rust-to-Slint navigation mapping. Page bodies remain explicit Slint
branches; the catalog is a maintenance aid, not an application framework.

## Storage model

The current app-scoped layout is:

```text
portaldata/
  apps/
    nexus/
      nexus.sqlite3
      settings.toml
      infrastructure.toml
      media/
        images/
        videos/
        audio/
      imports/
      temp/
  projects/
  shared/
```

Each persistent internal app should normally own one validated directory under
`portaldata/apps/<app-id>/`, obtained through `PortalPaths::app_dir(...)`.
`shared/` is opt-in and must be used only when ownership and schema are
explicit. Apps without durable state do not get empty directories.

Nexus stores metadata in SQLite and media as ordinary files. `settings.toml`
contains non-secret configuration. `infrastructure.toml` contains non-secret
RunPod resource identities and verification metadata. API keys and tokens
belong in the operating system keyring; the `RUNPOD_API_KEY` environment
variable is a non-persistent fallback. Secrets are not stored in app
directories or SQLite.

`PortalPaths::initialize` creates the Nexus directories and `shared/`. On
startup it migrates the older `portaldata/nexus/` directory to
`portaldata/apps/nexus/` only when the new location does not exist. If both
exist, the new location wins and the legacy directory is left untouched to
avoid destructive merging. The separate Home Project -> Shot workflow still
uses `portaldata/projects/`.

## Nexus domain model

- `Conversation` groups locally persisted `Message` rows and may reference a
  Project folder. A message has a user, assistant, or system role; current UI
  behavior persists user messages without fabricating unavailable AI output.
- `Character` holds a stable ID, name, description, notes, and timestamps.
  Character references associate a character with existing image
  `MediaAsset`s and a typed role; they do not duplicate the media file.
- `MediaAsset` is metadata for an ordinary local image, video, or audio file.
  It records a safe relative path, source, optional job/model, dimensions,
  duration, and MIME type. SQLite does not hold media BLOBs.
- `GenerationJob` is the durable lifecycle record. It holds intent-related
  fields, provider state, remote ID, output media ID, error, and available cost
  metrics. Its modeled states include Draft, Preparing, Queued, Running,
  Downloading, Completed, Failed, and Cancelled.
- `GenerationIntent` is Portal's backend-independent request: mode, model,
  user idea, compiled/manual prompt, parameters, characters, and references.
- `MediaWorkerRequest` / `MediaWorkerResponse` form the versioned boundary with
  a compute worker. Provider and ComfyUI details do not belong in durable user
  intent.

Five `GenerationMode` variants are modeled: TextToImage, ImageToImage,
TextToVideo, ImageToVideo, and VideoToVideo. Only FLUX.2 TextToImage is backed
by the current worker. Model profiles for FLUX.2, LTX-2.5, and Wan 2.2 express
validation and future capability metadata; a profile is not proof that a
backend exists.

## RunPod architecture

Portal deliberately separates RunPod's two services:

- `https://api.runpod.io` is the infrastructure REST API v2 surface used for
  Serverless endpoints, Network Volumes, GPU/data-center catalogs, and billing
  history.
- `https://api.runpod.ai/v2/{endpoint}/...` is the Serverless queue surface
  used to submit, poll, and cancel jobs.

Setup runs read-only local storage, GHCR, authentication, permission, catalog,
billing, and resource-discovery checks. Infrastructure follows **Discover ->
Plan -> Confirm -> Apply**. The plan reuses or creates
`portal-nexus-models`, and reuses, creates, or updates
`portal-nexus-flux2`. Its worker defaults are min 0 / max 1, but persistent
volume storage and generation compute remain billable.

The managed endpoint becomes authoritative for image generations; legacy
manual IDs are advanced fallbacks. Portal saves each durable resource identity
as provisioning succeeds, verifies the endpoint, and does not automatically
delete successfully created resources after a later failure. Infrastructure
creation and test generation require separate explicit confirmations.

Queue work runs on a standard background thread. Portal stores the remote job
ID before polling, tolerates transient poll errors, stops on terminal states,
and reconciles non-terminal jobs after restart. A provider-completed job is not
locally Completed until its artifact is validated, saved, registered in
SQLite, and linked to the job. Ingestion failure is durably Failed.

The safe diagnostic report uses an allow-list of version, OS, worker/resource
identity, paths, status, and provider-safe errors. It excludes prompts and chat
history and redacts API keys, bearer headers, HF tokens, and keyring contents.

## FLUX.2 worker

`portal-comfy-worker` is a RunPod Serverless handler around a loopback-only
ComfyUI process. It accepts contract version 1, validates only
`text_to_image` with model ID `flux-2-dev`, fills a fixed API-format workflow,
runs it, and returns a size-limited inline PNG. Portal chooses the final local
filename and owns the final output.

The first worker start bootstraps these source-defined files into the attached
Network Volume model cache:

- `diffusion_models/flux2_dev_fp8mixed.safetensors`
- `text_encoders/mistral_3_small_flux2_fp8.safetensors`
- `vae/flux2-vae.safetensors`

Downloads use `.part` files, exact expected byte sizes, atomic replacement,
and an advisory lock so concurrent starts do not race. No model weights are
stored in Git or baked into the worker image. The worker image remains
`ghcr.io/jakob-plappert/portal-comfy-worker:0.6.0` for Portal 0.8 because this
milestone does not alter worker runtime behavior.

## Security boundaries

- Portal/Rust is authoritative over UI actions, local files, SQLite, projects,
  conversations, jobs, confirmations, and provider resource IDs.
- An LLM never receives arbitrary shell or filesystem access.
- RunPod API keys stay in the OS keyring or process environment and are never
  written to SQLite, TOML settings, diagnostics, or documentation.
- Workers know only their versioned request and response contracts. They do
  not know Portal projects, conversations, characters, or database structure.
- Remote output kind, MIME type, header, and size are validated before local
  persistence. Remote filenames are treated only as extension hints and never
  trusted as local paths.
- Billable resource creation requires explicit confirmation after a plan.
  Generation compute requires its own confirmation in the first-run test flow.
- Successfully created provider resources are not deleted automatically on a
  partial failure.

## Important source file index

| Need to change | Inspect first |
| --- | --- |
| Internal app inventory or navigation mapping | `portal-app/src/app_catalog.rs`, `portal-app/src/main.rs`, `portal-app/ui/main.slint` |
| Global runtime state/startup | `portal-app/src/app_state.rs`, `portal-app/src/main.rs` |
| App-scoped paths or migration | `portal-app/src/paths.rs` |
| Home projects and shots | `portal-app/src/project.rs`, `portal-app/src/shot.rs`, `portal-app/src/main.rs` |
| Nexus callbacks and Rust/Slint projections | `portal-app/src/nexus_ui.rs` |
| Nexus SQLite/domain persistence | `portal-app/src/storage.rs` |
| Generation modes and intent | `portal-app/src/generation.rs`, `portal-app/src/model_catalog.rs` |
| Worker contract | `portal-app/src/worker_contract.rs` |
| Media import/remote ingestion | `portal-app/src/media_library.rs` |
| RunPod queue and restart reconciliation | `portal-app/src/runpod.rs` |
| RunPod REST v2 wire shapes | `portal-app/src/runpod_infrastructure.rs` |
| Discovery, planning, and confirmed apply | `portal-app/src/provisioning.rs` |
| Setup state, preflight, and diagnostics | `portal-app/src/setup.rs`, `portal-app/src/registry_preflight.rs` |
| Secrets and non-secret settings | `portal-app/src/settings.rs` |
| Slint layout and callbacks | `portal-app/ui/main.slint`, then the owning Rust setup function |
| FLUX worker behavior/model bootstrap | `portal-comfy-worker/handler.py`, `portal_worker.py`, `model_bootstrap.py`, `workflows/flux2_text_to_image.json` |
| RunPod operations and first-run troubleshooting | `docs/runpod-nexus.md` |
| Adding another internal app | `docs/APP_DEVELOPMENT_GUIDE.md` |

## Known limitations

- The real RunPod provisioning-to-visible-PNG path is ready for a human live
  test but is not verified by repository evidence.
- The remote worker implements only FLUX.2 TextToImage. ImageToImage, LTX, Wan,
  and all video generation paths have no worker implementation.
- The current worker transport embeds PNG data and caps it at 25 MiB; large
  video artifact transport is deferred.
- Prompt compiler modes and contracts exist, but there is no production local
  or remote compiler integration and no production conversational LLM.
- Portal has no automatic provider-resource deletion UI.
- The internal app catalog is compile-time by design; there is no dynamic
  plugin system.
- Home's older Project -> Shot data remains at `portaldata/projects/` rather
  than an app-scoped Home directory.

## Roadmap — non-authoritative / subject to change

- Human-verify the complete live FLUX.2 path and record only confirmed facts.
- Add production prompt compilation and conversational assistance while
  preserving Rust authority and narrow structured boundaries.
- Add image-guided and video workers only with explicit contracts, local final
  output ownership, and truthful capability status.
- Add future internal apps through the compile-time catalog and app-scoped
  storage conventions, not a plugin framework.

## Documentation maintenance contract

This document is maintained manually as part of relevant pull requests; it is
not a generated essay or a transcript. Source code remains authoritative.
Update this file in the same change whenever architecture, app inventory,
storage, capabilities, worker contracts, external integrations, or security
boundaries materially change. Use precise status language—**IMPLEMENTED**,
**PARTIAL**, **PLANNED**, and **LIVE-VERIFIED**—and never promote a capability
because it merely has a model, UI placeholder, or automated test.

Version, release milestone, and active worker tag are useful durable facts.
Avoid timestamps and chat history that create maintenance churn without
describing the product.

## How future ChatGPT/Codex sessions should start

Copy this bootstrap instruction:

> Use the GitHub repository jakob-plappert/portal as source of truth.
> First read PORTAL_CONTEXT.md and AGENTS.md.
> Then inspect the actual source files relevant to the task.
> Do not assume planned features are implemented.
> Preserve Portal's explicit internal-app architecture and app-scoped storage.
