//! RunPod account and infrastructure transport.
//!
//! RunPod exposes infrastructure management at `api.runpod.io`, while queued
//! inference uses `api.runpod.ai`. Keeping this beta API's wire shapes in this
//! module prevents provider field names from leaking into Nexus domain types.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::Duration;

pub const RUNPOD_INFRASTRUCTURE_BASE_URL: &str = "https://api.runpod.io";

// Deliberately no `Debug`: the derived representation would contain the API
// key. The client is moved into background threads but is never logged.
#[derive(Clone)]
pub struct RunPodInfrastructureClient {
    api_key: String,
    base_url: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkVolume {
    pub id: String,
    pub name: String,
    pub size: u32,
    #[serde(rename = "dataCenter")]
    pub data_center: String,
    #[serde(rename = "type")]
    pub volume_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub image: String,
    pub workers: EndpointWorkers,
    #[serde(rename = "dataCenterIds", default)]
    pub data_center_ids: Vec<String>,
    #[serde(rename = "networkVolumes", default)]
    pub network_volumes: Vec<String>,
    #[serde(default)]
    pub gpu: Option<EndpointGpu>,
    #[serde(default)]
    pub timeout: u64,
    #[serde(default)]
    pub disk: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointWorkers {
    pub min: u32,
    pub max: u32,
    #[serde(rename = "idleTimeout", default)]
    pub idle_timeout: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointGpu {
    pub pools: Vec<String>,
    #[serde(default = "one")]
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuType {
    pub id: String,
    pub name: String,
    pub pool: Option<String>,
    pub memory: u32,
    pub price: GpuPrice,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(rename = "dataCenters", default)]
    pub data_centers: Vec<GpuDataCenter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuPrice {
    pub secure: f64,
    pub community: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuDataCenter {
    pub id: String,
    pub name: String,
    pub availability: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataCenter {
    pub id: String,
    pub name: String,
    #[serde(rename = "networkVolumeTypes", default)]
    pub network_volume_types: Vec<String>,
}

/// Billing data is actual provider data. Only the documented aggregate total
/// used by this first UI is copied out of the provider response.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BillingSummary {
    pub total_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreateNetworkVolumeRequest {
    pub name: String,
    pub size: u32,
    #[serde(rename = "dataCenter")]
    pub data_center: String,
    #[serde(rename = "type")]
    pub volume_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CreateEndpointRequest {
    pub name: String,
    pub image: String,
    #[serde(rename = "type")]
    pub endpoint_type: String,
    pub gpu: EndpointGpu,
    pub workers: EndpointWorkers,
    pub scaling: EndpointScaling,
    #[serde(rename = "dataCenterIds")]
    pub data_center_ids: Vec<String>,
    #[serde(rename = "networkVolumes")]
    pub network_volumes: Vec<String>,
    pub timeout: u64,
    pub disk: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdateEndpointRequest {
    pub image: String,
    pub gpu: EndpointGpu,
    pub workers: EndpointWorkers,
    pub scaling: EndpointScaling,
    #[serde(rename = "dataCenterIds")]
    pub data_center_ids: Vec<String>,
    #[serde(rename = "networkVolumes")]
    pub network_volumes: Vec<String>,
    pub timeout: u64,
    pub disk: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EndpointScaling {
    #[serde(rename = "type")]
    pub scaling_type: String,
    #[serde(rename = "queueDelay")]
    pub queue_delay: u32,
}

impl RunPodInfrastructureClient {
    pub fn new(api_key: String) -> Result<Self, String> {
        if api_key.trim().is_empty() {
            return Err(String::from("RunPod API key is not configured."));
        }
        Ok(Self {
            api_key,
            base_url: String::from(RUNPOD_INFRASTRUCTURE_BASE_URL),
            agent: http_agent(),
        })
    }

    /// Listing volumes is a harmless authenticated read and therefore a safe
    /// way to validate both the key and its infrastructure permissions.
    pub fn validate_api_key(&self) -> Result<(), String> {
        self.list_network_volumes().map(|_| ())
    }

    pub fn list_endpoints(&self) -> Result<Vec<Endpoint>, String> {
        let response: wire::ListEndpointsResponse = self.get("/v2/serverless")?;
        Ok(response.endpoints)
    }

    pub fn get_endpoint(&self, id: &str) -> Result<Endpoint, String> {
        self.get(&format!("/v2/serverless/{}", checked_id(id)?))
    }

    pub fn create_endpoint(&self, request: &CreateEndpointRequest) -> Result<Endpoint, String> {
        self.post("/v2/serverless", request)
    }

    pub fn update_endpoint(
        &self,
        id: &str,
        request: &UpdateEndpointRequest,
    ) -> Result<Endpoint, String> {
        self.patch(&format!("/v2/serverless/{}", checked_id(id)?), request)
    }

    pub fn list_network_volumes(&self) -> Result<Vec<NetworkVolume>, String> {
        let response: wire::ListNetworkVolumesResponse = self.get("/v2/network-volumes")?;
        Ok(response.network_volumes)
    }

    pub fn get_network_volume(&self, id: &str) -> Result<NetworkVolume, String> {
        self.get(&format!("/v2/network-volumes/{}", checked_id(id)?))
    }

    pub fn create_network_volume(
        &self,
        request: &CreateNetworkVolumeRequest,
    ) -> Result<NetworkVolume, String> {
        self.post("/v2/network-volumes", request)
    }

    pub fn list_gpus(&self) -> Result<Vec<GpuType>, String> {
        let response: wire::ListGpusResponse =
            self.get("/v2/catalog/gpus?include=AVAILABILITY&product=SERVERLESS")?;
        Ok(response.gpus)
    }

    pub fn list_data_centers(&self) -> Result<Vec<DataCenter>, String> {
        let response: wire::ListDataCentersResponse =
            self.get("/v2/catalog/datacenters?include=GPU_AVAILABILITY")?;
        Ok(response.data_centers)
    }

    pub fn recent_billing(&self) -> Result<BillingSummary, String> {
        let response: wire::BillingResponse = self.get("/v2/billing?bucketSize=day&lastN=30")?;
        Ok(BillingSummary {
            total_usd: response.metadata.as_ref().and_then(wire::total_amount),
        })
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self
            .agent
            .get(format!("{}{}", self.base_url, path))
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .call()
            .map_err(provider_error)?;
        read_json(response, "read")
    }

    fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, String> {
        let response = self
            .agent
            .post(format!("{}{}", self.base_url, path))
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(body)
            .map_err(provider_error)?;
        read_json(response, "create")
    }

    fn patch<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, String> {
        let response = self
            .agent
            .patch(format!("{}{}", self.base_url, path))
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .send_json(body)
            .map_err(provider_error)?;
        read_json(response, "update")
    }
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
}

fn read_json<T: DeserializeOwned>(
    mut response: ureq::http::Response<ureq::Body>,
    operation: &str,
) -> Result<T, String> {
    response.body_mut().read_json().map_err(|error| {
        format!("RunPod returned an invalid infrastructure {operation} response: {error}")
    })
}

fn provider_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(401) => String::from("RunPod rejected the API key (HTTP 401)."),
        ureq::Error::StatusCode(403) => String::from(
            "The RunPod API key lacks permission for this infrastructure request (HTTP 403).",
        ),
        ureq::Error::StatusCode(404) => {
            String::from("The stored RunPod resource no longer exists (HTTP 404). Refresh setup.")
        }
        ureq::Error::StatusCode(409 | 422) => String::from(
            "RunPod rejected the infrastructure configuration as conflicting or invalid.",
        ),
        ureq::Error::StatusCode(429) => {
            String::from("RunPod rate-limited the request. Wait briefly and retry.")
        }
        ureq::Error::Timeout(_) => String::from("The RunPod infrastructure request timed out."),
        other => format!("RunPod infrastructure request failed: {other}"),
    }
}

fn checked_id(id: &str) -> Result<&str, String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(String::from(
            "RunPod resource ID contains invalid characters.",
        ));
    }
    Ok(id)
}

const fn one() -> u32 {
    1
}

mod wire {
    use serde::Deserialize;

    use super::{DataCenter, Endpoint, GpuType, NetworkVolume};

    #[derive(Deserialize)]
    pub struct ListEndpointsResponse {
        pub endpoints: Vec<Endpoint>,
    }

    #[derive(Deserialize)]
    pub struct ListNetworkVolumesResponse {
        #[serde(rename = "networkVolumes")]
        pub network_volumes: Vec<NetworkVolume>,
    }

    #[derive(Deserialize)]
    pub struct ListGpusResponse {
        pub gpus: Vec<GpuType>,
    }

    #[derive(Deserialize)]
    pub struct ListDataCentersResponse {
        #[serde(rename = "dataCenters")]
        pub data_centers: Vec<DataCenter>,
    }

    #[derive(Deserialize)]
    pub struct BillingResponse {
        pub metadata: Option<serde_json::Value>,
    }

    pub fn total_amount(value: &serde_json::Value) -> Option<f64> {
        // REST v2 billing metadata currently nests aggregate amounts. Accept
        // both documented aggregate spellings so an additive beta schema
        // revision does not turn a successful read into fake data.
        value
            .pointer("/totals/totalAmount")
            .or_else(|| value.get("totalAmount"))
            .and_then(serde_json::Value::as_f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infrastructure_and_queue_hosts_are_intentionally_different() {
        assert_eq!(RUNPOD_INFRASTRUCTURE_BASE_URL, "https://api.runpod.io");
        assert_ne!(
            RUNPOD_INFRASTRUCTURE_BASE_URL,
            crate::runpod::RUNPOD_QUEUE_BASE_URL
        );
    }

    #[test]
    fn current_discovery_fixture_parses() {
        let raw = r#"{
            "gpus": [{
                "id":"NVIDIA L40S", "name":"L40S", "pool":"ADA_48",
                "manufacturer":"NVIDIA", "memory":48,
                "secure":true, "community":true,
                "price":{"secure":0.8,"community":0.6},
                "maxCount":{"secure":8,"community":4},
                "availability":"HIGH",
                "dataCenters":[{"id":"EU-RO-1","name":"Romania","availability":"HIGH"}]
            }]
        }"#;
        let parsed: wire::ListGpusResponse =
            serde_json::from_str(raw).expect("official-shaped fixture should parse");
        assert_eq!(parsed.gpus[0].memory, 48);
        assert_eq!(parsed.gpus[0].price.secure, 0.8);
        assert_eq!(parsed.gpus[0].price.community, 0.6);
    }

    #[test]
    fn endpoint_listing_parses_current_shape_without_pagination() {
        let endpoints: wire::ListEndpointsResponse = serde_json::from_value(serde_json::json!({
            "endpoints": [{
                "id": "endpoint-1",
                "name": "portal-nexus-flux2",
                "image": "ghcr.io/example/worker:0.6.0",
                "workers": {"min": 0, "max": 1, "idleTimeout": 5},
                "dataCenterIds": ["EU-1"],
                "networkVolumes": ["volume-1"],
                "gpu": {"pools": ["ADA_48"], "count": 1},
                "timeout": 3600000,
                "disk": 20
            }]
        }))
        .expect("endpoint fixture should parse");
        assert_eq!(endpoints.endpoints[0].workers.min, 0);
    }

    #[test]
    fn network_volume_listing_parses_current_shape() {
        let volumes: wire::ListNetworkVolumesResponse = serde_json::from_value(serde_json::json!({
            "networkVolumes": [{
                "id": "volume-1", "name": "portal-nexus-models",
                "size": 150, "dataCenter": "EU-1", "type": "STANDARD"
            }]
        }))
        .expect("volume fixture should parse");
        assert_eq!(volumes.network_volumes[0].size, 150);
    }

    #[test]
    fn endpoint_create_request_scales_to_zero_and_attaches_volume() {
        let request = CreateEndpointRequest {
            name: String::from("portal-nexus-flux2"),
            image: String::from("ghcr.io/example/worker:0.6.0"),
            endpoint_type: String::from("QUEUE"),
            gpu: EndpointGpu {
                pools: vec![String::from("ADA_48")],
                count: 1,
            },
            workers: EndpointWorkers {
                min: 0,
                max: 1,
                idle_timeout: 5,
            },
            scaling: EndpointScaling {
                scaling_type: String::from("QUEUE_DELAY"),
                queue_delay: 4,
            },
            data_center_ids: vec![String::from("EU-RO-1")],
            network_volumes: vec![String::from("volume-1")],
            timeout: 900_000,
            disk: 20,
        };
        let json = serde_json::to_value(request).expect("request should serialize");
        assert_eq!(json["workers"]["min"], 0);
        assert_eq!(json["workers"]["max"], 1);
        assert_eq!(json["networkVolumes"][0], "volume-1");
    }

    #[test]
    fn volume_create_request_matches_rest_v2_field_names() {
        let json = serde_json::to_value(CreateNetworkVolumeRequest {
            name: String::from("portal-nexus-models"),
            size: 150,
            data_center: String::from("EU-RO-1"),
            volume_type: String::from("STANDARD"),
        })
        .expect("request should serialize");
        assert_eq!(json["dataCenter"], "EU-RO-1");
        assert_eq!(json["type"], "STANDARD");
    }

    #[test]
    fn billing_total_is_read_from_documented_metadata_totals() {
        let metadata = serde_json::json!({
            "query": {},
            "recordCount": 30,
            "totals": { "totalAmount": 12.34 }
        });
        assert_eq!(wire::total_amount(&metadata), Some(12.34));
    }
}
