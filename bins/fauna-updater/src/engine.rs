//! The Docker Engine calls the updater makes, behind a trait so the re-create
//! and the reconcile are unit-testable against a recording fake. Inspects and
//! the create body travel as the Engine API's own JSON.

use anyhow::{Context, Result};
use serde_json::Value;

/// The engine, as the updater uses it.
pub trait Engine {
    /// The ids of every container (running or not) carrying `label=value`.
    async fn labelled(&self, label: &str, value: &str) -> Result<Vec<String>>;
    async fn inspect_container(&self, id: &str) -> Result<Value>;
    async fn inspect_image(&self, reference: &str) -> Result<Value>;
    /// Pull `reference` (`<repo>@<digest>`), to completion.
    async fn pull(&self, reference: &str) -> Result<()>;
    /// Tag the image `source` as `<repo>:<tag>`.
    async fn tag(&self, source: &str, repo: &str, tag: &str) -> Result<()>;
    async fn rename(&self, id: &str, name: &str) -> Result<()>;
    /// Stop within `grace` seconds (the container's own when `None`).
    async fn stop(&self, id: &str, grace: Option<i64>) -> Result<()>;
    /// Create a container named `name` from `body`; its id.
    async fn create(&self, name: &str, body: Value) -> Result<String>;
    async fn start(&self, id: &str) -> Result<()>;
    async fn remove_container(&self, id: &str) -> Result<()>;
    async fn remove_image(&self, id: &str) -> Result<()>;
}

/// The engine on the local socket.
pub struct Bollard(bollard::Docker);

impl Bollard {
    pub fn connect() -> Result<Self> {
        Ok(Self(
            bollard::Docker::connect_with_socket_defaults()
                .context("connect to the Docker socket")?,
        ))
    }
}

impl Engine for Bollard {
    async fn labelled(&self, label: &str, value: &str) -> Result<Vec<String>> {
        use bollard::query_parameters::ListContainersOptionsBuilder;
        let filter = format!("{label}={value}");
        let filters = std::collections::HashMap::from([("label".to_string(), vec![filter])]);
        let opts = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        Ok(self
            .0
            .list_containers(Some(opts))
            .await?
            .into_iter()
            .filter_map(|c| c.id)
            .collect())
    }

    async fn inspect_container(&self, id: &str) -> Result<Value> {
        let inspect = self
            .0
            .inspect_container(
                id,
                None::<bollard::query_parameters::InspectContainerOptions>,
            )
            .await?;
        Ok(serde_json::to_value(inspect)?)
    }

    async fn inspect_image(&self, reference: &str) -> Result<Value> {
        Ok(serde_json::to_value(
            self.0.inspect_image(reference).await?,
        )?)
    }

    async fn pull(&self, reference: &str) -> Result<()> {
        use futures_util::TryStreamExt;
        let opts = bollard::query_parameters::CreateImageOptionsBuilder::default()
            .from_image(reference)
            .build();
        self.0
            .create_image(Some(opts), None, None)
            .try_collect::<Vec<_>>()
            .await?;
        Ok(())
    }

    async fn tag(&self, source: &str, repo: &str, tag: &str) -> Result<()> {
        let opts = bollard::query_parameters::TagImageOptionsBuilder::default()
            .repo(repo)
            .tag(tag)
            .build();
        self.0.tag_image(source, Some(opts)).await?;
        Ok(())
    }

    async fn rename(&self, id: &str, name: &str) -> Result<()> {
        let opts = bollard::query_parameters::RenameContainerOptionsBuilder::default()
            .name(name)
            .build();
        self.0.rename_container(id, opts).await?;
        Ok(())
    }

    async fn stop(&self, id: &str, grace: Option<i64>) -> Result<()> {
        let mut opts = bollard::query_parameters::StopContainerOptionsBuilder::default();
        if let Some(t) = grace {
            opts = opts.t(t as i32);
        }
        self.0.stop_container(id, Some(opts.build())).await?;
        Ok(())
    }

    async fn create(&self, name: &str, body: Value) -> Result<String> {
        let opts = bollard::query_parameters::CreateContainerOptionsBuilder::default()
            .name(name)
            .build();
        let body: bollard::models::ContainerCreateBody =
            serde_json::from_value(body).context("the successor's create body")?;
        Ok(self.0.create_container(Some(opts), body).await?.id)
    }

    async fn start(&self, id: &str) -> Result<()> {
        self.0
            .start_container(id, None::<bollard::query_parameters::StartContainerOptions>)
            .await?;
        Ok(())
    }

    async fn remove_container(&self, id: &str) -> Result<()> {
        self.0
            .remove_container(
                id,
                None::<bollard::query_parameters::RemoveContainerOptions>,
            )
            .await?;
        Ok(())
    }

    async fn remove_image(&self, id: &str) -> Result<()> {
        self.0
            .remove_image(
                id,
                None::<bollard::query_parameters::RemoveImageOptions>,
                None,
            )
            .await?;
        Ok(())
    }
}

/// A recording engine for the unit tests: answers inspects from fixtures (by
/// id or by name, as the engine does), logs every mutating call, fails the
/// call named in `fail`.
#[cfg(test)]
pub mod fake {
    use super::Engine;
    use anyhow::{Context, Result};
    use serde_json::{Value, json};
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct Fake {
        pub containers: Vec<Value>,
        pub calls: Mutex<Vec<String>>,
        pub bodies: Mutex<Vec<Value>>,
        pub fail: Option<&'static str>,
    }

    impl Fake {
        fn log(&self, call: String) -> Result<()> {
            let fails = self.fail.is_some_and(|f| call.starts_with(f));
            self.calls.lock().unwrap().push(call);
            if fails {
                anyhow::bail!("injected")
            } else {
                Ok(())
            }
        }
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        /// The last create body.
        pub fn created(&self) -> Value {
            self.bodies.lock().unwrap().last().cloned().unwrap()
        }
    }

    impl Engine for Fake {
        async fn labelled(&self, _: &str, _: &str) -> Result<Vec<String>> {
            Ok(self
                .containers
                .iter()
                .map(|c| c["Id"].as_str().unwrap().to_string())
                .collect())
        }
        async fn inspect_container(&self, id: &str) -> Result<Value> {
            self.containers
                .iter()
                .find(|c| c["Id"] == id || c["Name"] == format!("/{id}"))
                .cloned()
                .context("no such container")
        }
        async fn inspect_image(&self, reference: &str) -> Result<Value> {
            Ok(json!({"Id": format!("id-of-{reference}")}))
        }
        async fn pull(&self, r: &str) -> Result<()> {
            self.log(format!("pull {r}"))
        }
        async fn tag(&self, s: &str, r: &str, t: &str) -> Result<()> {
            self.log(format!("tag {s} {r}:{t}"))
        }
        async fn rename(&self, id: &str, n: &str) -> Result<()> {
            self.log(format!("rename {id} {n}"))
        }
        async fn stop(&self, id: &str, g: Option<i64>) -> Result<()> {
            self.log(format!("stop {id} {g:?}"))
        }
        async fn create(&self, n: &str, b: Value) -> Result<String> {
            self.log(format!("create {n} {}", b["Image"].as_str().unwrap()))?;
            self.bodies.lock().unwrap().push(b);
            Ok("new".into())
        }
        async fn start(&self, id: &str) -> Result<()> {
            self.log(format!("start {id}"))
        }
        async fn remove_container(&self, id: &str) -> Result<()> {
            self.log(format!("rm {id}"))
        }
        async fn remove_image(&self, id: &str) -> Result<()> {
            self.log(format!("rmi {id}"))
        }
    }
}
