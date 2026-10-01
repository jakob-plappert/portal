"""RunPod Serverless entry point for Portal's FLUX.2 image worker."""

from __future__ import annotations

import base64
import os
from pathlib import Path
import subprocess
import threading

import runpod

from model_bootstrap import bootstrap_models
from portal_worker import (
    MAX_INLINE_IMAGE_BYTES,
    load_workflow,
    run_comfy_workflow,
    validate_request,
    wait_for_comfy,
)

COMFY_URL = "http://127.0.0.1:8188"
WORKER_DIR = Path(__file__).resolve().parent
WORKFLOW_PATH = WORKER_DIR / "workflows" / "flux2_text_to_image.json"
OUTPUT_DIR = Path(os.environ.get("COMFYUI_OUTPUT_DIR", "/tmp/portal-output"))
_comfy_process: subprocess.Popen | None = None
_comfy_lock = threading.Lock()


def start_comfy() -> subprocess.Popen:
    model_root = Path(os.environ.get("NEXUS_MODEL_ROOT", "/runpod-volume/models"))
    bootstrap_models(model_root, os.environ.get("HF_TOKEN"))
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    comfy_dir = Path(os.environ.get("COMFYUI_DIR", "/opt/ComfyUI"))
    process = subprocess.Popen(
        [
            "/opt/venv/bin/python",
            str(comfy_dir / "main.py"),
            "--listen",
            "127.0.0.1",
            "--port",
            "8188",
            "--output-directory",
            str(OUTPUT_DIR),
            "--extra-model-paths-config",
            str(comfy_dir / "extra_model_paths.yaml"),
        ],
        cwd=comfy_dir,
    )
    wait_for_comfy(COMFY_URL)
    return process


def ensure_comfy_started() -> None:
    """Bootstrap models and ComfyUI once, after the RunPod handler is live."""
    global _comfy_process
    # RunPod may execute handler calls on worker threads. The lock keeps two
    # first jobs from starting duplicate downloads or ComfyUI processes even
    # if endpoint concurrency is increased in a later release.
    with _comfy_lock:
        if _comfy_process is not None and _comfy_process.poll() is None:
            return
        _comfy_process = start_comfy()


def handle_job(job: dict) -> dict:
    try:
        ensure_comfy_started()
        request = validate_request(job.get("input"))
        workflow = load_workflow(WORKFLOW_PATH, request)
        image_path, execution_ms = run_comfy_workflow(COMFY_URL, workflow, OUTPUT_DIR)
        image = image_path.read_bytes()
        image_path.unlink(missing_ok=True)
        if len(image) > MAX_INLINE_IMAGE_BYTES:
            raise RuntimeError(
                "Generated PNG exceeds the inline transport limit; "
                "a temporary artifact URL is required."
            )
        return {
            "status": "completed",
            "artifacts": [
                {
                    "kind": "image",
                    "mime_type": "image/png",
                    "filename": "flux2-output.png",
                    "content": {
                        "transport": "inline_base64",
                        "data": base64.b64encode(image).decode("ascii"),
                    },
                }
            ],
            "metadata": {
                "model_id": "flux-2-dev",
                "engine": "ComfyUI",
                "execution_time_ms": execution_ms,
            },
            "error": None,
        }
    except Exception as error:
        return {
            "status": "failed",
            "artifacts": [],
            "metadata": {"model_id": "flux-2-dev", "engine": "ComfyUI"},
            "error": str(error),
        }


if __name__ == "__main__":
    # Register with RunPod immediately. The first accepted job performs the
    # potentially long model bootstrap inside the endpoint's job timeout
    # rather than leaving the platform waiting for a handler to appear.
    runpod.serverless.start({"handler": handle_job})
