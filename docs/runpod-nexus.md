# Nexus and RunPod deployment design

Nexus treats RunPod as temporary compute. Portal owns projects, conversations,
characters, job history, and media metadata locally. Every completed remote
artifact must be downloaded into `portaldata/nexus/media`, registered in the
local SQLite database, and displayed from that local copy. A remote artifact
can be cleaned up after successful receipt.

No FLUX, LTX, or Wan model is deployed by this milestone. The designs below
describe the next deployment work.

## Compute strategies

### A. Custom Serverless endpoints (recommended long-term default)

Use separate image, video, and prompt-LLM endpoints. Min/flex workers can scale
to zero when idle. Model weights, ComfyUI, required custom nodes, caches, and
temporary workflow assets live on an attached network volume so cold workers do
not redownload everything.

Portal submits a stable versioned request to the appropriate endpoint, polls
the queue job, downloads each output, and saves it locally. Endpoint IDs remain
developer-configurable initially. A later milestone can provision or discover
endpoints so a normal user only supplies a RunPod API key.

### B. Development GPU Pod (recommended for initial debugging)

Start a GPU Pod with the network volume attached, run ComfyUI interactively,
and test FLUX.2 [dev], LTX-2.5, and Wan 2.2 workflows before packaging a
serverless worker. Stop or terminate compute when debugging is finished while
retaining weights and caches on network storage. This provides much faster
workflow inspection than debugging only through queue responses.

### C. RunPod public endpoints

Public endpoints are operationally simplest and may require only an API key,
but Portal is limited to the hosted models, parameters, and policy offered by
those endpoints. They may not expose the exact open-weight or ComfyUI workflow
required by Nexus. Portal should still download results into its local media
library rather than treating provider storage as permanent.

## Storage responsibilities

The network volume should mainly contain:

- model weights;
- ComfyUI and custom nodes;
- model/download caches;
- temporary workflow assets when necessary.

Permanent generated images, videos, and audio belong on the user's local
computer under `portaldata/nexus/media`. SQLite contains metadata and relative
paths only; it does not contain media BLOBs.

## Stable future worker contract

Portal sends a versioned media request containing:

- generation mode and stable model ID;
- compiled prompt and optional negative prompt;
- optional seed;
- generic dimensions and duration where applicable;
- temporary remote references when the selected mode requires input media.

The worker translates that contract into model-specific ComfyUI workflows. Raw
workflow JSON and node IDs do not cross the public Portal-side boundary.

The response reports a status, useful non-secret metadata, errors, and zero or
more artifacts. Each artifact identifies its media kind and MIME type when
known. Small outputs may use inline base64. Large images and especially videos
must use temporary downloadable URLs or object storage so JSON bodies do not
carry large base64 payloads. Portal then streams the artifact to a normal local
file, inserts its `MediaAsset` metadata, links it to the generation job, and can
allow the temporary remote object to expire.

## Video and audio

The contract represents true video outputs such as MP4 for text-to-video,
image-to-video, and video-to-video work. Model profiles separately describe no
audio, generated synchronized audio, and external audio input. Audio synthesis,
speech, lip-sync, and model-specific multiplexing remain responsibilities of a
future RunPod worker; they are not local Portal pipelines.

## Next deployment sequence

1. Build and debug a FLUX.2 [dev] workflow on a development GPU Pod.
2. Package it behind the versioned contract on a custom image endpoint.
3. Submit from Nexus and download a real result into the local Media Library.
4. Add LTX-2.5 and/or Wan 2.2 video workers without changing Portal's stable
   generation intent or worker contract.
