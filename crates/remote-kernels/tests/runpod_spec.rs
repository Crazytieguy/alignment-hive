//! Validate our hand-written `RunPod` REST v2 types against the vendored
//! official OpenAPI spec (`tests/runpod-v2-openapi.json`, fetched verbatim
//! from <https://api.runpod.io/v2/openapi.json> on 2026-08-18). Mirrors
//! `tests/vast_spec.rs`: every field we *send* must exist in the spec, and
//! every response shape we parse comes from the spec's own examples, so a
//! spec refresh that drifts fails here instead of in production.

use std::collections::{BTreeSet, HashMap};

use remote_kernels::config::Config;
use remote_kernels::runpod::client::Problem;
use remote_kernels::runpod::types::{self, Pod};
use remote_kernels::runtime::ProvisionRequest;
use remote_kernels::runtime::runpod::RunPodRuntime;

fn spec() -> serde_json::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/runpod-v2-openapi.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Resolve a `#/components/schemas/...` pointer.
fn resolve<'a>(spec: &'a serde_json::Value, reference: &str) -> &'a serde_json::Value {
    let name = reference
        .strip_prefix("#/components/schemas/")
        .unwrap_or_else(|| panic!("unsupported $ref {reference}"));
    &spec["components"]["schemas"][name]
}

/// Collect a schema's property names, following `allOf`/`$ref` transitively.
fn collect_props(
    spec: &serde_json::Value,
    schema: &serde_json::Value,
    seen: &mut BTreeSet<String>,
    out: &mut BTreeSet<String>,
) {
    if let Some(reference) = schema["$ref"].as_str() {
        if !seen.insert(reference.to_string()) {
            return;
        }
        collect_props(spec, resolve(spec, reference), seen, out);
        return;
    }
    if let Some(props) = schema["properties"].as_object() {
        out.extend(props.keys().cloned());
    }
    if let Some(all_of) = schema["allOf"].as_array() {
        for sub in all_of {
            collect_props(spec, sub, seen, out);
        }
    }
}

fn resolve_props(spec: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_props(
        spec,
        &spec["components"]["schemas"][schema_name],
        &mut BTreeSet::new(),
        &mut out,
    );
    assert!(
        !out.is_empty(),
        "schema {schema_name:?} resolved to no properties"
    );
    out
}

/// Collect a schema's `required` names, following `allOf`/`$ref`.
fn collect_required(
    spec: &serde_json::Value,
    schema: &serde_json::Value,
    seen: &mut BTreeSet<String>,
    out: &mut BTreeSet<String>,
) {
    if let Some(reference) = schema["$ref"].as_str() {
        if !seen.insert(reference.to_string()) {
            return;
        }
        collect_required(spec, resolve(spec, reference), seen, out);
        return;
    }
    if let Some(required) = schema["required"].as_array() {
        out.extend(required.iter().filter_map(|v| v.as_str().map(String::from)));
    }
    if let Some(all_of) = schema["allOf"].as_array() {
        for sub in all_of {
            collect_required(spec, sub, seen, out);
        }
    }
}

fn required_props(spec: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_required(
        spec,
        &spec["components"]["schemas"][schema_name],
        &mut BTreeSet::new(),
        &mut out,
    );
    out
}

fn enum_values(spec: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
    spec["components"]["schemas"][schema_name]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("schema {schema_name:?} has no enum"))
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect()
}

/// The single example the spec documents for a response — no hand-copied
/// fixtures, so these cannot drift from the vendored spec.
fn example(
    spec: &serde_json::Value,
    path: &str,
    method: &str,
    status: &str,
    content_type: &str,
) -> serde_json::Value {
    let response = &spec["paths"][path][method]["responses"][status];
    let response = if let Some(reference) = response["$ref"].as_str() {
        let name = reference
            .strip_prefix("#/components/responses/")
            .unwrap_or_else(|| panic!("unsupported response $ref {reference}"));
        &spec["components"]["responses"][name]
    } else {
        response
    };
    let examples = response["content"][content_type]["examples"]
        .as_object()
        .unwrap_or_else(|| panic!("no examples for {method} {path} {status}"));
    examples.values().next().expect("at least one example")["value"].clone()
}

fn pod_example(spec: &serde_json::Value) -> serde_json::Value {
    example(spec, "/v2/pods/{id}", "get", "200", "application/json")
}

fn runtime_with(config_toml: &str) -> RunPodRuntime {
    let config: Config = toml::from_str(config_toml).unwrap();
    RunPodRuntime::new("test-key".to_string(), &config)
}

fn provision_request() -> ProvisionRequest {
    ProvisionRequest {
        machine_id: "m1".to_string(),
        gpu_type: None,
        image: None,
        vast_offers: None,
        priority: None,
        env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
        ssh_public_key: "ssh-ed25519 AAAA test".to_string(),
        jupyter_token: "tok".to_string(),
        cleanup: remote_kernels::config::Cleanup::Terminate,
    }
}

/// Assert every key of `value` (an object) exists in the resolved spec
/// properties of `schema_name`.
fn assert_keys_in_schema(
    spec: &serde_json::Value,
    value: &serde_json::Value,
    schema_name: &str,
    what: &str,
) {
    let allowed = resolve_props(spec, schema_name);
    for key in value
        .as_object()
        .unwrap_or_else(|| panic!("{what} is not an object"))
        .keys()
    {
        assert!(
            allowed.contains(key),
            "{what} field {key:?} is not a property of {schema_name} \
             (spec has: {allowed:?})"
        );
    }
}

#[test]
fn create_pod_body_fields_exist_in_spec() {
    let spec = spec();

    // A maximal body: gpu, disk, ports, env, args (the orphan guard), a
    // persistent mount, and the v1 passthroughs we still accept.
    let rt = runtime_with(
        r#"
        [runpod]
        data-center-ids = ["EU-RO-1"]
        allowed-cuda-versions = ["12.8"]
        container-registry-auth-id = "cr_1"
        "#,
    );
    let (body, _note) = rt
        .pod_create_request(&provision_request(), "NVIDIA GeForce RTX 4090")
        .unwrap();
    let json = serde_json::to_value(&body).unwrap();

    assert_keys_in_schema(&spec, &json, "CreatePodRequest", "CreatePodRequest");
    assert_keys_in_schema(&spec, &json["gpu"], "CreateGpuConfig", "gpu");
    assert_keys_in_schema(&spec, &json["mounts"], "Mounts", "mounts");
    assert_keys_in_schema(
        &spec,
        &json["mounts"]["persistent"],
        "PersistentMount",
        "mounts.persistent",
    );
    assert!(json["args"].is_string(), "args must serialize as a string");

    // The network-mount shape is the other half of `Mounts`.
    let rt = runtime_with("[runpod]\nnetwork-volume-id = \"vol_xyz\"");
    let (body, _note) = rt
        .pod_create_request(&provision_request(), "NVIDIA GeForce RTX 4090")
        .unwrap();
    let json = serde_json::to_value(&body).unwrap();
    assert_keys_in_schema(&spec, &json, "CreatePodRequest", "CreatePodRequest");
    assert_keys_in_schema(&spec, &json["mounts"], "Mounts", "mounts");
    assert_keys_in_schema(
        &spec,
        &json["mounts"]["network"][0],
        "NetworkMount",
        "mounts.network[0]",
    );
}

#[test]
fn create_pod_field_whitelist_matches_spec() {
    let spec = spec();
    let spec_fields = resolve_props(&spec, "CreatePodRequest");
    let ours: BTreeSet<String> = types::CREATE_POD_FIELDS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        ours, spec_fields,
        "CREATE_POD_FIELDS drifted from the vendored spec's CreatePodRequest"
    );
    assert_eq!(spec_fields.len(), 16, "expected 16 v2 create properties");

    let managed: BTreeSet<String> = types::MANAGED_CREATE_FIELDS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert!(
        managed.is_subset(&ours) && managed.len() < ours.len(),
        "MANAGED_CREATE_FIELDS must be a strict subset of CREATE_POD_FIELDS"
    );
}

#[test]
fn create_pod_required_fields_are_present() {
    let spec = spec();
    let rt = runtime_with("");
    let (body, _note) = rt
        .pod_create_request(&provision_request(), "NVIDIA GeForce RTX 4090")
        .unwrap();
    let json = serde_json::to_value(&body).unwrap();

    let required = required_props(&spec, "CreatePodRequest");
    assert!(
        required.contains("name"),
        "spec drift: name must be required"
    );
    for field in &required {
        assert!(
            !json[field.as_str()].is_null(),
            "required CreatePodRequest field {field:?} is missing from our body"
        );
    }
    // `image` is required unless templateId is set, and we never send one.
    assert!(json["image"].is_string(), "image must always be sent");
    // The one required property of the GPU block.
    for field in required_props(&spec, "CreateGpuConfig") {
        assert!(
            !json["gpu"][field.as_str()].is_null(),
            "required CreateGpuConfig field {field:?} is missing"
        );
    }
}

#[test]
fn endpoints_and_action_enum_exist_in_spec() {
    let spec = spec();
    for (path, method) in [
        ("/v2/pods", "post"),
        ("/v2/pods", "get"),
        ("/v2/pods/{id}", "get"),
        ("/v2/pods/{id}", "delete"),
        ("/v2/pods/{id}/action", "post"),
    ] {
        assert!(
            !spec["paths"][path][method].is_null(),
            "{method} {path} missing from the vendored spec"
        );
    }
    assert_eq!(
        required_props(&spec, "PodActionRequest"),
        BTreeSet::from(["action".to_string()])
    );
    let actions = enum_values(&spec, "PodAction");
    for action in ["start", "stop", "terminate"] {
        assert!(actions.contains(action), "PodAction lost {action:?}");
    }
    assert!(
        required_props(&spec, "ListPodsResponse").contains("pods"),
        "ListPodsResponse must require the pods key we deserialize"
    );
}

#[test]
fn pod_status_constants_match_spec() {
    let spec = spec();
    let ours: BTreeSet<String> = types::POD_STATUSES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(ours, enum_values(&spec, "PodStatus"));
}

#[test]
fn cloud_values_match_spec() {
    let spec = spec();
    let ours: BTreeSet<String> = types::CLOUDS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(ours, enum_values(&spec, "Cloud"));
    assert_eq!(
        ours,
        BTreeSet::from(["SECURE".to_string(), "COMMUNITY".to_string()])
    );
}

#[test]
fn pod_running_example_deserializes() {
    let spec = spec();
    let pod: Pod = serde_json::from_value(pod_example(&spec)).unwrap();
    assert_eq!(pod.status.as_deref(), Some("RUNNING"));
    assert!(pod.is_running());
    assert_eq!(pod.hourly_cost(), Some(0.44));
    assert_eq!(pod.gpu_display_name(), "NVIDIA GeForce RTX 4090");
    assert_eq!(pod.data_center_id.as_deref(), Some("US-KS-2"));
    assert_eq!(
        pod.direct_ssh(),
        Some(("195.26.233.3".to_string(), 34446_u16))
    );
    assert!(pod.has_proxy_port());
    assert_eq!(
        pod.runtime.as_ref().unwrap().ports[0].private,
        Some(22),
        "runtime port mapping must survive the parse"
    );
}

#[test]
fn pod_provisioning_example_deserializes() {
    let spec = spec();
    let value = example(&spec, "/v2/pods", "post", "201", "application/json");
    assert!(value["ssh"]["direct"].is_null(), "spec drift: 201 example");
    assert!(value["startedAt"].is_null(), "spec drift: 201 example");
    let pod: Pod = serde_json::from_value(value).unwrap();
    assert_eq!(pod.status.as_deref(), Some("PROVISIONING"));
    assert!(pod.direct_ssh().is_none());
    assert_eq!(pod.hourly_cost(), Some(0.44));
}

#[test]
fn exited_pod_reports_no_rate() {
    let spec = spec();
    let mut value = pod_example(&spec);
    value["status"] = serde_json::json!("EXITED");
    value["cost"] = serde_json::json!(0.0);
    value["ssh"] = serde_json::json!({"proxy": null, "direct": null});
    value["runtime"] = serde_json::Value::Null;
    let pod: Pod = serde_json::from_value(value).unwrap();
    assert_eq!(pod.status.as_deref(), Some("EXITED"));
    // 0.0 must NOT zero the ledger's recorded rate (D8).
    assert_eq!(pod.hourly_cost(), None);
    assert!(pod.direct_ssh().is_none());
    assert!(!pod.is_running());
}

#[test]
fn unknown_status_and_unknown_fields_deserialize() {
    let spec = spec();
    let mut value = pod_example(&spec);
    value["status"] = serde_json::json!("HIBERNATING");
    value["somethingNew"] = serde_json::json!({"nested": [1, 2, 3]});
    let pod: Pod = serde_json::from_value(value).unwrap();
    // A provider status we've never seen must never break describe().
    assert_eq!(pod.status.as_deref(), Some("HIBERNATING"));
    assert!(!pod.is_running());
}

#[test]
fn problem_json_error_is_parsed_and_rendered() {
    use remote_kernels::runpod::client::RunPodError;

    let spec = spec();
    let not_found = example(
        &spec,
        "/v2/pods/{id}",
        "get",
        "404",
        "application/problem+json",
    );
    let problem = Problem::parse(&not_found.to_string()).expect("404 example must parse");
    assert_eq!(problem.status, Some(404));
    assert_eq!(problem.detail.as_deref(), Some("resource not found"));
    let rendered = RunPodError::Api {
        status: 404,
        body: not_found.to_string(),
    }
    .to_string();
    assert!(rendered.contains("resource not found"), "{rendered}");

    // The 422 example carries the schema's documented `errors` list.
    let mut unprocessable = example(&spec, "/v2/pods", "post", "422", "application/problem+json");
    let errors = spec["components"]["schemas"]["ErrorResponse"]["properties"]["errors"]["examples"]
        [0]
    .clone();
    assert!(
        errors.is_array(),
        "spec drift: ErrorResponse.errors example"
    );
    unprocessable["errors"] = errors;
    let problem = Problem::parse(&unprocessable.to_string()).expect("422 example must parse");
    assert_eq!(problem.status, Some(422));
    assert_eq!(problem.errors.len(), 1);
    let rendered = RunPodError::Api {
        status: 422,
        body: unprocessable.to_string(),
    }
    .to_string();
    assert!(
        rendered.contains("Request validation failed."),
        "{rendered}"
    );
    assert!(
        rendered.contains("additional properties 'bogus' not allowed"),
        "the per-field violations are what makes a 422 actionable: {rendered}"
    );

    // A non-JSON body still renders something a user can act on.
    let rendered = RunPodError::Api {
        status: 502,
        body: "<html>bad gateway</html>".to_string(),
    }
    .to_string();
    assert!(rendered.contains("bad gateway"), "{rendered}");
    assert!(Problem::parse("<html>bad gateway</html>").is_none());
}

#[test]
fn list_response_wrapper_deserializes() {
    let spec = spec();
    let listed = serde_json::json!({ "pods": [pod_example(&spec)] });
    let parsed: types::ListPodsResponse = serde_json::from_value(listed).unwrap();
    assert_eq!(parsed.pods.len(), 1);
    assert_eq!(parsed.pods[0].name.as_deref(), Some("pytorch-training"));
}
