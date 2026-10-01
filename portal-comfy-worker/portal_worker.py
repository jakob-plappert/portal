"""Portal's narrow MediaWorkerRequest to ComfyUI FLUX.2 adapter."""

from __future__ import annotations

import copy
import json
from pathlib import Path
import secrets
import time
import urllib.error
import urllib.request

REQUEST_VERSION = 1
MODEL_ID = "flux-2-dev"
MAX_INLINE_IMAGE_BYTES = 25 * 1024 * 1024


def validate_request(value: object) -> dict:
    if not isinstance(value, dict):
        raise ValueError("Worker input must be a JSON object.")
    if value.get("request_version") != REQUEST_VERSION:
        raise ValueError("Unsupported MediaWorkerRequest version.")
    if value.get("generation_mode") != "text_to_image":
        raise ValueError("This worker currently supports only TextToImage.")
    if value.get("model_id") != MODEL_ID:
        raise ValueError("This endpoint serves only the flux-2-dev model ID.")
    prompt = value.get("compiled_prompt")
    if not isinstance(prompt, str) or not prompt.strip():
        raise ValueError("A non-empty compiled prompt is required.")
    if len(prompt) > 20_000:
        raise ValueError("The compiled prompt exceeds the worker limit.")
    dimensions = value.get("dimensions")
    if not isinstance(dimensions, dict):
        raise ValueError("Image dimensions are required.")
    for name in ("width", "height"):
        dimension = dimensions.get(name)
        if (
            isinstance(dimension, bool)
            or not isinstance(dimension, int)
            or not 256 <= dimension <= 2048
        ):
            raise ValueError(f"{name} must be an integer from 256 through 2048.")
        if dimension % 16:
            raise ValueError(f"{name} must be divisible by 16 for FLUX.2.")
    seed = value.get("seed")
    if seed is not None and (
        isinstance(seed, bool) or not isinstance(seed, int) or not 0 <= seed < 2**63
    ):
        raise ValueError("seed must be a non-negative 63-bit integer.")
    return value


def load_workflow(workflow_path: Path, request: dict) -> dict:
    with workflow_path.open("r", encoding="utf-8") as source:
        workflow = json.load(source)
    workflow = copy.deepcopy(workflow)
    dimensions = request["dimensions"]
    seed = request.get("seed")
    workflow["6"]["inputs"]["text"] = request["compiled_prompt"].strip()
    workflow["25"]["inputs"]["noise_seed"] = seed if seed is not None else secrets.randbits(63)
    for node_id in ("47", "48"):
        workflow[node_id]["inputs"]["width"] = dimensions["width"]
        workflow[node_id]["inputs"]["height"] = dimensions["height"]
    return workflow


def run_comfy_workflow(
    base_url: str,
    workflow: dict,
    output_dir: Path,
    timeout_seconds: int = 840,
) -> tuple[Path, int]:
    started = time.monotonic()
    submitted = _post_json(f"{base_url}/prompt", {"prompt": workflow})
    prompt_id = submitted.get("prompt_id")
    if not isinstance(prompt_id, str) or not prompt_id:
        raise RuntimeError("ComfyUI rejected the fixed FLUX.2 API workflow.")
    while time.monotonic() - started < timeout_seconds:
        history = _get_json(f"{base_url}/history/{prompt_id}")
        entry = history.get(prompt_id)
        if entry:
            status = entry.get("status", {})
            if status.get("status_str") == "error":
                raise RuntimeError("ComfyUI reported a workflow execution error.")
            images = entry.get("outputs", {}).get("9", {}).get("images", [])
            if images:
                filename = Path(images[0]["filename"]).name
                image_path = output_dir / filename
                if not image_path.is_file():
                    raise RuntimeError("ComfyUI completed but its PNG was not found.")
                return image_path, int((time.monotonic() - started) * 1000)
        time.sleep(2)
    raise TimeoutError("FLUX.2 worker timed out waiting for ComfyUI.")


def wait_for_comfy(base_url: str, timeout_seconds: int = 120) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        try:
            _get_json(f"{base_url}/system_stats")
            return
        except (OSError, ValueError, urllib.error.URLError):
            time.sleep(1)
    raise TimeoutError("ComfyUI did not become ready inside the worker.")


def _post_json(url: str, payload: dict) -> dict:
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url, data=body, headers={"Content-Type": "application/json"}, method="POST"
    )
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def _get_json(url: str) -> dict:
    with urllib.request.urlopen(url, timeout=30) as response:
        return json.load(response)
