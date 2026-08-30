//! One arm's ClickHouse container, via `testcontainers-modules`' own
//! `ClickHouse` image: RAII cleanup (the container is removed when the
//! handle drops, success, error or panic) and a readiness check proven
//! against the real image, rather than a hand-rolled `docker rm` and wait
//! loop at every exit path. Orchestration of the server the driver talks
//! to, not computation on emulated state: outside PURITY.md's framing
//! entirely, the same category `make up`'s own `docker compose up`
//! already is.

use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::clickhouse::ClickHouse;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("starting {0}: {1}")]
    Start(String, testcontainers::TestcontainersError),
    #[error("reading {0}'s mapped port: {1}")]
    Port(String, testcontainers::TestcontainersError),
}

/// One running arm's container. Dropping this stops and removes it.
pub struct Container {
    _handle: ContainerAsync<ClickHouse>,
    pub http_port: u16,
}

/// Starts a fresh container from `image` (`name:tag` or `name@sha256:...`,
/// tag or digest only -- the image name itself is always
/// `clickhouse/clickhouse-server`), waiting until its HTTP interface
/// answers.
pub async fn start(image_ref: &str) -> Result<Container, DockerError> {
    let tag = image_ref
        .strip_prefix("clickhouse/clickhouse-server:")
        .or_else(|| image_ref.strip_prefix("clickhouse/clickhouse-server@"))
        .unwrap_or(image_ref);
    let handle = ClickHouse::default()
        .with_tag(tag)
        .with_env_var("CLICKHOUSE_PASSWORD", "clickdoom")
        .start()
        .await
        .map_err(|e| DockerError::Start(image_ref.to_string(), e))?;

    let http_port = handle
        .get_host_port_ipv4(8123)
        .await
        .map_err(|e| DockerError::Port(image_ref.to_string(), e))?;

    Ok(Container {
        _handle: handle,
        http_port,
    })
}
