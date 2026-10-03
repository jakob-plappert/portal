# Nexus and RunPod

Nexus treats RunPod as temporary compute. Portal remains authoritative for
projects, conversations, characters, generation history, and final media. A
successful remote result is downloaded into
`portaldata/apps/nexus/media/images`, written as an ordinary file, and only then
registered in SQLite. The Network Volume is not permanent user-media storage.

This repository implements discovery, planning, confirmed provisioning, queue
polling, local ingestion, and the first FLUX.2 worker. It has not created live
infrastructure or completed a real remote generation as part of automated
development.

## First-run RunPod setup

1. Enter a RunPod API key in Portal Settings. Portal stores it in the operating
   system keyring, never `settings.toml` or SQLite.
2. Choose **Run Preflight**. These checks are read-only and non-billable. Nexus
   authenticates the key, checks required infrastructure reads, follows GHCR's
   anonymous Bearer-token challenge to verify the exact public worker tag,
   probes local database/media/temp storage, discovers existing endpoints and
   volumes, reads the live GPU/data-center catalogs, and attempts to read recent
   billing history. Missing billing visibility is a warning when all required
   provisioning reads work.
3. Choose **Plan Infrastructure** and review the separate Network Volume,
   Serverless endpoint, and cost sections. The plan says whether each resource
   will be reused, created, or updated and shows GPU, VRAM, location, worker
   image, worker limits, reference Pod rates, the unavailable live Serverless
   rate, and requested volume size.
4. Choose **Create Infrastructure** to explicitly confirm billable writes.
   Preflight and planning cannot issue create or update requests.
5. Nexus creates or reuses `portal-nexus-models`, creates or updates
   `portal-nexus-flux2`, and verifies the endpoint through the provider API.
   Portal shows each stage and stores successful IDs in `infrastructure.toml`
   after each durable step.
6. Choose **Generate Test Image**, edit the prompt if desired, read the separate
   compute-charge/model-bootstrap warning, and choose **Generate Test Image**
   again to confirm. Infrastructure confirmation is not generation permission.
7. Wait for the first model bootstrap. Portal reports truthful coarse states
   because RunPod does not expose a reliable model-download percentage. The
   first worker may download approximately 54 GB of FLUX.2 artifacts to the
   attached volume; later workers validate and reuse them.
8. Confirm the PNG appears in Nexus Media as a real image preview. Portal polls
   off the Slint thread, saves the PNG under
   `portaldata/apps/nexus/media/images`, inserts `MediaAsset` metadata in
   SQLite, and marks the job complete only after both file and database work
   succeed. Nexus is then ready for normal TextToImage generations.

If volume creation succeeds but endpoint creation fails, Nexus keeps and records
the volume. A retry rediscovers and reuses it; Portal does not aggressively
delete a billable resource that may already contain data.

This automated milestone does not claim a successful real end-to-end image.
That is proven only after a human completes the confirmed test, the PNG exists
locally, SQLite references it, and Nexus visibly renders it.

## Common first-run failures

- **Private or unavailable GHCR image:** Preflight checks
  `ghcr.io/jakob-plappert/portal-comfy-worker:0.6.0` anonymously. A Bearer
  challenge is normal; failure after the anonymous token exchange means the
  package/tag visibility or registry availability must be fixed before
  provisioning.
- **Invalid RunPod key:** HTTP 401 appears under **RunPod API**. Save the correct
  key in the OS keyring and rerun preflight.
- **Insufficient permissions:** Portal reports whether endpoint, Network
  Volume, GPU catalog, or data-center reads failed. Billing-only HTTP 403 is a
  warning; missing provisioning reads are blocking failures.
- **No compatible 48+ GB GPU:** Retry when Serverless capacity is available or
  inspect RunPod regions. Nexus requires a compatible GPU in a data center with
  STANDARD Network Volume support.
- **Volume created but endpoint failed:** Keep the volume. Portal saved or can
  rediscover its ID, and retry reuses it instead of creating a duplicate.
- **Worker cold start:** A queued or `IN_PROGRESS` job may be waiting for a
  worker/container. Portal intentionally shows no fake percentage.
- **Model bootstrap failure:** Inspect the job error and copy the safe
  diagnostic report. Confirm the Network Volume is attached, large enough, and
  writable; then retry without deleting a successfully created volume.
- **Local disk write failure:** Fix the reported Nexus media/temp path or disk
  condition and rerun preflight. A provider-completed job whose PNG cannot be
  ingested is stored locally as **Failed**, never **Completed** or permanently
  **Downloading**.

## Two RunPod APIs

RunPod exposes two intentionally separate services:

- `https://api.runpod.io/v2` is the REST API v2 infrastructure surface. Nexus
  uses its documented Serverless, Network Volume, GPU catalog, data center, and
  billing endpoints. Provider wire structures are isolated in
  `runpod_infrastructure.rs` because this API is beta. Request and response
  fields follow RunPod's current
  [OpenAPI schema](https://api.runpod.io/v2/openapi.json).
- `https://api.runpod.ai/v2/{endpoint_id}/...` is the Serverless queue surface.
  Portal uses `POST run`, `GET status/{job_id}`, and `POST cancel/{job_id}`.

The REST v2 responses Portal currently relies on expose neither a supported
current account-credit/balance field nor a live Serverless rate Portal can use
for a defensible estimate. Nexus says this explicitly. Billing totals are
labeled actual historical spend. GPU catalog
`secure` and `community` prices are labeled Pod reference rates and are never
used as claimed Serverless costs. Portal cannot calculate a defensible
per-image estimate until a real Serverless rate source is available.

## Provisioned resources and cost behavior

The default FLUX-first plan requests a configurable 150 GB STANDARD Network
Volume named `portal-nexus-models`. The size is deliberately shown before
creation: storage remains billable even when compute is idle. The volume holds
model weights and reusable caches and leaves headroom for later model work; it
does not silently allocate a very large video-model volume.

GPU selection uses the live catalog. The Balanced policy requires at least 48
GB VRAM, a Serverless pool, current availability, and a data center that also
supports STANDARD Network Volumes. It prioritizes availability and uses the
documented secure Pod rate only as a tie-breaker—not as a Serverless price.
Existing volumes constrain the choice to their data center so idempotent reuse
remains possible. The user's local GPU is irrelevant to remote placement.

The image endpoint is named `portal-nexus-flux2`, attaches the selected volume,
uses one GPU, and defaults to `workers.min = 0`, `workers.max = 1`. Idle GPU
compute can therefore scale to zero, but Network Volume storage continues to
cost money. A one-hour request timeout permits a slow initial container pull and
roughly 54 GB model bootstrap; warm generations should be much shorter. Portal
does not expose automatic delete actions yet.

## FLUX.2 worker

`portal-comfy-worker` is a narrow compute adapter. It knows only the stable
`MediaWorkerRequest`, a fixed versioned ComfyUI API workflow, and the
`MediaWorkerResponse`; it has no Portal project, conversation, character, or
SQLite knowledge. ComfyUI listens only on worker loopback in production.

The worker pins a ComfyUI commit and uses the current
[Comfy-Org FLUX.2 [dev] split artifacts](https://huggingface.co/Comfy-Org/flux2-dev):

- `flux2_dev_fp8mixed.safetensors` diffusion model;
- `mistral_3_small_flux2_fp8.safetensors` FLUX.2 text encoder;
- `flux2-vae.safetensors` VAE.

This FP8/mixed configuration follows the current Comfy-Org workflow and avoids
the original full-precision checkpoint. It is not FLUX.1 and Portal never
silently substitutes FLUX.1. Files are downloaded on first worker startup to
`/runpod-volume/models`, checked by exact expected size, written through `.part`
files, and atomically renamed while an advisory lock prevents concurrent first
downloads. No weights are included in Git or the container image.

The Comfy-Org artifacts are downloadable without a token at the time of this
implementation, so zero-extra-credential provisioning does not require an HF
token. Their model card points to the original
[Black Forest Labs FLUX.2 [dev]](https://huggingface.co/black-forest-labs/FLUX.2-dev)
license; users must comply with those terms. The bootstrap supports an optional
worker-side `HF_TOKEN` environment variable if legitimate access later requires
one, and never logs it. Portal does not bypass gated access or license terms.

The first transport returns a size-limited inline PNG. Portal validates media
kind/MIME type, generates its own local filename, limits remote/inline sizes,
and never trusts a worker filename as a path. Large video work will use a
temporary object URL instead of JSON base64.

## Worker image distribution

`.github/workflows/portal-comfy-worker.yml` builds pull requests without
publishing. Main, worker tags, and manual runs publish version, commit, and
main/latest tags from `portal-comfy-worker/` to:

```text
ghcr.io/jakob-plappert/portal-comfy-worker
```

The workflow uses GitHub's provided `GITHUB_TOKEN`; no registry secret is
committed. The GHCR package must be made **public** in GitHub package settings
before RunPod can pull it anonymously. GitHub Actions cannot reliably enforce
that repository/package visibility setting. The default provisioning image is
the immutable milestone tag `0.6.0`.

## Local storage and security

```text
portaldata/
  apps/
    nexus/
      nexus.sqlite3
      settings.toml
      infrastructure.toml
      media/{images,videos,audio}/
      imports/
      temp/
  shared/
```

`infrastructure.toml` stores only non-secret resource identity and verification
metadata: volume/endpoint IDs and names, size, data center, selected GPU pool,
worker image, an optional Serverless-rate snapshot when a supported source
exists, and verification time. The RunPod key remains in the OS keyring. SQLite
contains metadata and relative paths, never media BLOBs or secrets.

## Compute alternatives

### A. Custom Serverless endpoints (implemented default)

Nexus provisions an image endpoint now; later releases can add separate video
and remote-expert LLM endpoints. Workers scale to zero, weights live on a
Network Volume, and outputs are downloaded locally. This is the intended
long-term experience.

### B. Development GPU Pod

For workflow debugging, start a Pod with the same Network Volume attached, run
ComfyUI interactively, validate the workflow, then stop or terminate compute.
Weights remain on network storage. This remains the recommended way to diagnose
new ComfyUI/video workflows before publishing a Serverless worker.

### C. RunPod public endpoints

Public endpoints are operationally simpler but limited to RunPod's hosted
models, parameters, and policies. They may not expose Nexus's exact open-weight
workflow. Portal would still download results locally.

## Local conversational AI direction

The future local Nexus brain is configured conceptually as:

- **Fast:** Qwen3.5-9B;
- **Quality:** Qwen3.5-35B-A3B;
- **Auto:** Nexus chooses between local modes;
- **Remote Expert:** optional RunPod LLM only when useful.

Portal/Rust remains the authority over files, SQLite, and actions. The LLM gets
small structured tools, never arbitrary filesystem or shell access. Only
selected relevant context may reach a remote expert, and local Nexus storage
remains the source of truth. This milestone does not download or run Qwen.

## Still deliberately deferred

- a verified live RunPod provisioning and PNG generation run;
- automatic RunPod endpoint deletion;
- temporary object storage for large artifacts;
- ImageToImage and all video modes in the worker;
- production prompt compilation and Qwen downloads;
- LTX/Wan workflows, synchronized audio, LoRA, face recognition, and editing.
