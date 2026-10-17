//! The successor's configuration, built from the running container's inspect —
//! the pure half of the rolling re-create (`update-channel.md` § The updater →
//! *What it does on Apply*): same environment, ports, volumes, labels, restart
//! policy, stop grace period and network attachments, on the new image.
//!
//! It works on the Engine API's own JSON shapes (an inspect in, a create body
//! out), so a fixture is a real `docker inspect` and the rule reads like the
//! API it targets. What the old IMAGE contributed is dropped from the copy
//! (an image's env, labels, command and the rest are its own defaults, which
//! the new image supplies afresh — copying them would pin the old image's
//! version label onto the successor), and so is what the engine minted for
//! the old container (its id-derived hostname and alias, its addresses).

use serde_json::{Map, Value, json};

/// The `Config` keys whose value, when it is the image's own, is the image's
/// to supply.
const IMAGE_DEFAULTED: [&str; 7] = [
    "Cmd",
    "Entrypoint",
    "WorkingDir",
    "User",
    "StopSignal",
    "Healthcheck",
    "Shell",
];

/// The create body for a successor of `container` (an inspect) running
/// `image_reference`, where `old_image` is the inspect of the image the
/// container runs now.
pub fn successor_body(container: &Value, old_image: &Value, image_reference: &str) -> Value {
    let empty = Map::new();
    let config = container["Config"].as_object().unwrap_or(&empty);
    let image_config = old_image["Config"].as_object().unwrap_or(&empty);
    let short_id: String = container["Id"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(12)
        .collect();

    let mut body = config.clone();
    body.insert("Image".into(), json!(image_reference));

    if body.get("Hostname").and_then(Value::as_str) == Some(short_id.as_str()) {
        body.remove("Hostname");
    }
    for key in IMAGE_DEFAULTED {
        if body.get(key).is_some() && body.get(key) == image_config.get(key) {
            body.remove(key);
        }
    }
    if let Some(Value::Array(env)) = body.get_mut("Env") {
        let image_env = image_config.get("Env").and_then(Value::as_array);
        env.retain(|e| !image_env.is_some_and(|ie| ie.contains(e)));
    }
    for key in ["Labels", "ExposedPorts", "Volumes"] {
        if let Some(Value::Object(map)) = body.get_mut(key) {
            let image_map = image_config.get(key).and_then(Value::as_object);
            map.retain(|k, v| image_map.and_then(|m| m.get(k)) != Some(v));
        }
    }

    body.insert("HostConfig".into(), container["HostConfig"].clone());

    let network_mode = container["HostConfig"]["NetworkMode"]
        .as_str()
        .unwrap_or("");
    let shares_a_namespace = network_mode == "host" || network_mode.starts_with("container:");
    if !shares_a_namespace {
        let mut endpoints = Map::new();
        if let Some(networks) = container["NetworkSettings"]["Networks"].as_object() {
            for (name, endpoint) in networks {
                let aliases: Vec<Value> = endpoint["Aliases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|a| a.as_str() != Some(short_id.as_str()))
                    .cloned()
                    .collect();
                let mut kept = Map::new();
                kept.insert("Aliases".into(), Value::Array(aliases));
                for key in ["IPAMConfig", "Links", "DriverOpts"] {
                    if !endpoint[key].is_null() {
                        kept.insert(key.into(), endpoint[key].clone());
                    }
                }
                endpoints.insert(name.clone(), Value::Object(kept));
            }
        }
        body.insert(
            "NetworkingConfig".into(),
            json!({ "EndpointsConfig": Value::Object(endpoints) }),
        );
    }
    Value::Object(body)
}

/// The container's name without the engine's leading `/`.
pub fn container_name(container: &Value) -> String {
    container["Name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('/')
        .to_string()
}

/// The grace period the container was created with (compose's
/// `stop_grace_period` lands as `Config.StopTimeout`), in seconds.
pub fn stop_timeout(container: &Value) -> Option<i64> {
    container["Config"]["StopTimeout"].as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container id: the 12-hex short form the engine derives the hostname
    /// and alias from, padded to the 64 hex an id has.
    fn id() -> String {
        format!("4f1c2d3e5a6b{}", "0".repeat(52))
    }

    /// The shape `docker inspect` gives for the compose bundle's nest.
    fn nest_inspect(network_mode: &str) -> Value {
        json!({
            "Id": id(),
            "Name": "/fauna-fauna-nest-1",
            "Config": {
                "Hostname": "4f1c2d3e5a6b",
                "Image": "ghcr.io/faunasocial/nest:test",
                "Env": ["FAUNA_MODE=public", "FAUNA_PORT=3000", "PATH=/usr/local/bin:/usr/bin"],
                "Entrypoint": ["/init"],
                "Cmd": null,
                "User": "",
                "StopTimeout": 15,
                "ExposedPorts": {"443/tcp": {}, "8080/tcp": {}},
                "Labels": {
                    "social.fauna.update": "true",
                    "com.docker.compose.service": "fauna-nest",
                    "org.opencontainers.image.version": "0.3.0",
                    "org.opencontainers.image.revision": "old"
                }
            },
            "HostConfig": {
                "NetworkMode": network_mode,
                "RestartPolicy": {"Name": "unless-stopped"},
                "PortBindings": {"443/tcp": [{"HostPort": "443"}]},
                "Binds": null,
                "Mounts": [{"Type": "volume", "Source": "fauna_fauna-data", "Target": "/data"}]
            },
            "NetworkSettings": {
                "Networks": {
                    "fauna_default": {
                        "Aliases": ["fauna-fauna-nest-1", "fauna-nest", "4f1c2d3e5a6b"],
                        "IPAddress": "172.18.0.3",
                        "EndpointID": "ep",
                        "NetworkID": "net",
                        "IPAMConfig": null
                    }
                }
            }
        })
    }

    fn old_image() -> Value {
        json!({
            "Id": "sha256:oldconfig",
            "Config": {
                "Env": ["PATH=/usr/local/bin:/usr/bin"],
                "Entrypoint": ["/init"],
                "User": "",
                "ExposedPorts": {"443/tcp": {}, "8080/tcp": {}},
                "Labels": {
                    "org.opencontainers.image.version": "0.3.0",
                    "org.opencontainers.image.revision": "old"
                }
            }
        })
    }

    #[test]
    fn the_successor_keeps_the_containers_own_configuration_on_the_new_image() {
        let body = successor_body(
            &nest_inspect("fauna_default"),
            &old_image(),
            "ghcr.io/faunasocial/nest:test",
        );
        assert_eq!(body["Image"], "ghcr.io/faunasocial/nest:test");
        assert_eq!(body["Env"], json!(["FAUNA_MODE=public", "FAUNA_PORT=3000"]));
        assert_eq!(
            body["Labels"],
            json!({"social.fauna.update": "true", "com.docker.compose.service": "fauna-nest"}),
            "the old image's version label must not ride onto the successor"
        );
        assert_eq!(body["StopTimeout"], 15);
        assert_eq!(
            body["HostConfig"]["RestartPolicy"]["Name"],
            "unless-stopped"
        );
        assert_eq!(body["HostConfig"]["Mounts"][0]["Target"], "/data");
        assert!(
            body.get("Hostname").is_none(),
            "the id-derived hostname is the engine's"
        );
        assert!(
            body.get("Entrypoint").is_none(),
            "the image's entrypoint is the image's"
        );
        assert!(body.get("ExposedPorts").is_some_and(|p| p == &json!({})));
        assert_eq!(
            body["NetworkingConfig"]["EndpointsConfig"]["fauna_default"],
            json!({"Aliases": ["fauna-fauna-nest-1", "fauna-nest"]}),
            "the service alias survives; the address and the id alias are the engine's"
        );
    }

    #[test]
    fn a_host_networked_container_gets_no_endpoint_config() {
        let body = successor_body(
            &nest_inspect("host"),
            &old_image(),
            "ghcr.io/faunasocial/nest:latest",
        );
        assert!(body.get("NetworkingConfig").is_none());
        assert_eq!(body["HostConfig"]["NetworkMode"], "host");
    }

    #[test]
    fn an_explicit_override_of_an_image_default_is_kept() {
        let mut inspect = nest_inspect("fauna_default");
        inspect["Config"]["Entrypoint"] = json!(["/usr/local/bin/fauna-updater"]);
        let body = successor_body(&inspect, &old_image(), "ghcr.io/faunasocial/nest:dev");
        assert_eq!(body["Entrypoint"], json!(["/usr/local/bin/fauna-updater"]));
    }

    #[test]
    fn name_and_grace_period() {
        let inspect = nest_inspect("fauna_default");
        assert_eq!(container_name(&inspect), "fauna-fauna-nest-1");
        assert_eq!(stop_timeout(&inspect), Some(15));
    }
}
