"""One-time, lock-protected FLUX.2 model bootstrap on the Network Volume."""

from __future__ import annotations

import fcntl
import os
from pathlib import Path
import urllib.request

HF_BASE = "https://huggingface.co/Comfy-Org/flux2-dev/resolve/main/split_files"

# Current Comfy-Org FLUX.2 [dev] files, not FLUX.1 weights. Exact byte sizes
# provide an inexpensive integrity guard without hashing ~54 GB every startup.
MODEL_FILES = (
    (
        "diffusion_models/flux2_dev_fp8mixed.safetensors",
        f"{HF_BASE}/diffusion_models/flux2_dev_fp8mixed.safetensors",
        35_455_599_592,
    ),
    (
        "text_encoders/mistral_3_small_flux2_fp8.safetensors",
        f"{HF_BASE}/text_encoders/mistral_3_small_flux2_fp8.safetensors",
        18_034_640_095,
    ),
    (
        "vae/flux2-vae.safetensors",
        f"{HF_BASE}/vae/flux2-vae.safetensors",
        336_213_556,
    ),
)


def bootstrap_models(root: Path, token: str | None = None) -> None:
    """Ensure all required files exist while only one worker may download."""
    root.mkdir(parents=True, exist_ok=True)
    lock_path = root / ".portal-flux2-bootstrap.lock"
    with lock_path.open("a+b") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        for relative, url, expected_size in MODEL_FILES:
            destination = root / relative
            if destination.is_file() and destination.stat().st_size == expected_size:
                print(f"Reusing {relative} from the Network Volume.")
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            temporary = destination.with_suffix(destination.suffix + ".part")
            print(f"Downloading required FLUX.2 file: {relative}")
            request = urllib.request.Request(url)
            if token:
                # The token is added only to Hugging Face and is never printed.
                request.add_header("Authorization", f"Bearer {token}")
            try:
                with urllib.request.urlopen(request, timeout=120) as response:
                    with temporary.open("wb") as output:
                        while chunk := response.read(8 * 1024 * 1024):
                            output.write(chunk)
                actual_size = temporary.stat().st_size
                if actual_size != expected_size:
                    raise RuntimeError(
                        f"Downloaded {relative} has {actual_size} bytes; "
                        f"expected {expected_size}."
                    )
                os.replace(temporary, destination)
            except Exception:
                temporary.unlink(missing_ok=True)
                raise
