import json
from pathlib import Path
import unittest

from portal_worker import load_workflow, validate_request


WORKFLOW = Path(__file__).parent / "workflows" / "flux2_text_to_image.json"


def request():
    return {
        "request_version": 1,
        "generation_mode": "text_to_image",
        "model_id": "flux-2-dev",
        "compiled_prompt": "rainy Tokyo at night",
        "negative_prompt": None,
        "seed": 42,
        "dimensions": {"width": 1024, "height": 768},
        "duration_seconds": None,
        "references": [],
    }


class PortalWorkerTests(unittest.TestCase):
    def test_contract_maps_only_stable_fields_to_internal_nodes(self):
        value = validate_request(request())
        workflow = load_workflow(WORKFLOW, value)
        self.assertEqual(workflow["6"]["inputs"]["text"], "rainy Tokyo at night")
        self.assertEqual(workflow["25"]["inputs"]["noise_seed"], 42)
        self.assertEqual(workflow["47"]["inputs"]["height"], 768)
        self.assertNotIn("workflow", value)

    def test_worker_rejects_modes_not_implemented_by_flux_endpoint(self):
        value = request()
        value["generation_mode"] = "image_to_image"
        with self.assertRaisesRegex(ValueError, "only TextToImage"):
            validate_request(value)

    def test_workflow_is_comfyui_api_format(self):
        workflow = json.loads(WORKFLOW.read_text(encoding="utf-8"))
        self.assertEqual(workflow["12"]["class_type"], "UNETLoader")
        self.assertEqual(
            workflow["12"]["inputs"]["unet_name"],
            "flux2_dev_fp8mixed.safetensors",
        )
        self.assertNotIn("nodes", workflow)


if __name__ == "__main__":
    unittest.main()
