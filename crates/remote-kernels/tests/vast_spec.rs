//! Validate our hand-written vast.ai types against the vendored official
//! OpenAPI spec (tests/vast-openapi.yaml, from
//! docs.vast.ai/api-reference/openapi.yaml). The spec lacks operationIds so
//! codegen isn't viable; this test is the "still validated against the pinned
//! spec" half of that tradeoff: every field we *send* must exist in the spec.

use std::collections::HashMap;

use remote_kernels::vast::types::CreateInstanceRequest;

fn spec() -> serde_yaml::Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vast-openapi.yaml");
    serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn request_properties(spec: &serde_yaml::Value, path: &str, method: &str) -> Vec<String> {
    spec["paths"][path][method]["requestBody"]["content"]["application/json"]["schema"]
        ["properties"]
        .as_mapping()
        .unwrap_or_else(|| panic!("no request properties for {method} {path}"))
        .keys()
        .filter_map(|k| k.as_str().map(String::from))
        .collect()
}

#[test]
fn create_instance_fields_exist_in_spec() {
    let spec = spec();
    let allowed = request_properties(&spec, "/api/v0/asks/{id}/", "put");

    let full = CreateInstanceRequest {
        image: "img".to_string(),
        disk: 40.0,
        runtype: "ssh".to_string(),
        label: Some("l".to_string()),
        env: remote_kernels::vast::types::docker_env_flags(&HashMap::from([(
            "FOO".to_string(),
            "bar".to_string(),
        )]))
        .unwrap(),
        onstart: Some("#!/bin/bash\n".to_string()),
        vm: Some(true),
        template_hash_id: Some("h".to_string()),
        extra: HashMap::new(),
    };
    let json = serde_json::to_value(&full).unwrap();
    for key in json.as_object().unwrap().keys() {
        assert!(
            allowed.contains(key),
            "CreateInstanceRequest field {key:?} is not in the spec's create-instance \
             properties; API drift? Spec has: {allowed:?}"
        );
    }
    // The spec types `env` as a Docker-flags STRING, not a map.
    assert!(json["env"].is_string(), "env must serialize as a string");
    assert_eq!(json["env"], "-e FOO='bar'");
}

#[test]
fn docker_env_flags_rejects_hazards() {
    use remote_kernels::vast::types::docker_env_flags;
    assert!(docker_env_flags(&HashMap::new()).unwrap().is_none());
    assert!(docker_env_flags(&HashMap::from([("BAD KEY".to_string(), "v".to_string())])).is_err());
    assert!(docker_env_flags(&HashMap::from([("K".to_string(), "it's bad".to_string())])).is_err());
    let multi = docker_env_flags(&HashMap::from([
        ("B".to_string(), "2".to_string()),
        ("A".to_string(), "1".to_string()),
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(multi, "-e A='1' -e B='2'"); // deterministic ordering
}

#[test]
fn offer_search_filter_fields_exist_in_spec() {
    let spec = spec();
    let allowed = request_properties(&spec, "/api/v0/bundles/", "post");

    // The default filters the runtime always sends (see VastRuntime::offer_filters)
    // plus the search controls the client adds.
    for field in [
        "verified",
        "reliability",
        "num_gpus",
        "gpu_name",
        "dph_total",
        "vms_enabled",
        "type",
        "rentable",
        "limit",
        "order",
    ] {
        assert!(
            allowed.contains(&field.to_string()),
            "offer filter {field:?} is not in the spec's bundles properties"
        );
    }
}

#[test]
fn instance_endpoints_exist_in_spec() {
    let spec = spec();
    for (path, method) in [
        ("/api/v1/instances/", "get"),
        ("/api/v0/instances/{id}/", "put"),
        ("/api/v0/instances/{id}/", "delete"),
        ("/api/v0/users/current/", "get"),
        ("/api/v0/ssh/", "post"),
        ("/api/v0/ssh/", "get"),
        ("/api/v0/ssh/{id}/", "delete"),
        ("/api/v0/instances/{id}/ssh/", "post"),
    ] {
        assert!(
            !spec["paths"][path][method].is_null(),
            "{method} {path} missing from the vendored spec"
        );
    }
}
