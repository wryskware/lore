//! Finding and calling the Lore daemon over loopback HTTP.
//!
//! Discovery happens on **every** tool call, not once at startup: an MCP server
//! is a long-lived stdio child of an editor, and the daemon it proxies is
//! restarted freely (new port, new pid). Caching the handshake would strand the
//! process on a dead port for the rest of the session, which is exactly the
//! failure an agent cannot diagnose. Reading a ~150-byte JSON file per call is
//! cheaper than being wrong.
//!
//! M1 posture: this crate never starts the daemon. Spawning a background
//! indexer as a side effect of an agent's search is not a decision an MCP tool
//! gets to make (D-0007: the daemon is the single authoritative owner of index
//! state); the failure path tells the agent to ask the user instead.

use camino::Utf8PathBuf;
use lore_core::discovery;
use lore_core::{
    ApiError, DaemonStatus, ExpandRequest, ExpandResponse, SearchRequest, SearchResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Where the daemon's `/v1` base URL comes from.
#[derive(Debug, Clone)]
pub enum Endpoint {
    /// Read `daemon.json` from this data directory on every call. Production.
    DataDir(Utf8PathBuf),
    /// A fixed `…/v1` base URL, skipping the handshake entirely. Used by the
    /// golden harness to point the server at a stub daemon.
    Fixed(String),
}

impl Endpoint {
    /// The platform data directory, honoring `LORE_DATA_DIR`.
    pub fn discovered() -> anyhow::Result<Self> {
        Ok(Self::DataDir(discovery::data_dir()?))
    }

    fn base_url(&self) -> Result<String, DaemonError> {
        let data_dir = match self {
            Endpoint::Fixed(url) => return Ok(url.clone()),
            Endpoint::DataDir(data_dir) => data_dir,
        };

        // A corrupt handshake is reported as "not running": the file is the
        // daemon's own output, so a client that cannot parse it has no port to
        // try and no better advice to give than "restart it".
        let handshake = match discovery::read(data_dir) {
            Ok(Some(handshake)) => handshake,
            Ok(None) => {
                return Err(DaemonError::NotRunning(format!(
                    "no daemon handshake at {}",
                    discovery::handshake_path(data_dir)
                )));
            }
            Err(err) => {
                return Err(DaemonError::NotRunning(format!(
                    "unreadable daemon handshake: {err:#}"
                )));
            }
        };

        // Version skew is a distinct failure from absence: retrying will not
        // fix it, and the user needs to know which half to upgrade.
        if handshake.api_version != lore_core::API_VERSION {
            return Err(DaemonError::Incompatible {
                daemon: handshake.api_version,
                client: lore_core::API_VERSION,
                daemon_version: handshake.daemon_version,
            });
        }

        // Staleness is *not* a veto — a heartbeat can lag a busy daemon that is
        // still answering. It is recorded so the error, if the call does fail,
        // says which of the two things went wrong.
        Ok(handshake.base_url())
    }

    /// Is the published heartbeat recent enough to believe on its own?
    /// `None` when there is no readable record to judge.
    fn heartbeat_is_fresh(&self) -> Option<bool> {
        match self {
            Endpoint::Fixed(_) => None,
            Endpoint::DataDir(data_dir) => discovery::read(data_dir)
                .ok()
                .flatten()
                .map(|handshake| discovery::is_fresh(&handshake, discovery::unix_now())),
        }
    }
}

/// Everything that can go wrong between "an agent asked" and "the daemon
/// answered". The `Display` text is what reaches the agent verbatim, so each
/// variant ends in an action rather than a diagnosis.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(
        "the lore daemon is not running ({0}). Ask the user to start it with `lore daemon`, then retry."
    )]
    NotRunning(String),

    #[error(
        "the lore daemon published a handshake but is not answering at {url} ({source}). It may have crashed or be shutting down; ask the user to (re)start it with `lore daemon`."
    )]
    Unreachable {
        url: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error(
        "the lore daemon published a stale handshake and is not answering at {url} ({source}). It is probably dead; ask the user to start it with `lore daemon`."
    )]
    Stale {
        url: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error(
        "the running lore daemon ({daemon_version}) speaks API v{daemon}, but this MCP proxy speaks v{client}. Ask the user to update lore so both halves match."
    )]
    Incompatible {
        daemon: u32,
        client: u32,
        daemon_version: String,
    },

    /// The daemon answered, and said no. Its message is already user-facing.
    #[error("the lore daemon rejected the request ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("the lore daemon returned a response this proxy could not parse: {0}")]
    Protocol(String),
}

/// A thin HTTP client. Holds no index state and caches nothing but the
/// connection pool.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    endpoint: Endpoint,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            http: reqwest::Client::new(),
        }
    }

    pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, DaemonError> {
        self.post("search", request).await
    }

    pub async fn expand(&self, request: &ExpandRequest) -> Result<ExpandResponse, DaemonError> {
        self.post("expand", request).await
    }

    pub async fn status(&self) -> Result<DaemonStatus, DaemonError> {
        let url = format!("{}/status", self.endpoint.base_url()?);
        let response = self.send(self.http.get(&url), &url).await?;
        decode(response).await
    }

    async fn post<Q: Serialize, R: DeserializeOwned>(
        &self,
        route: &str,
        body: &Q,
    ) -> Result<R, DaemonError> {
        let url = format!("{}/{route}", self.endpoint.base_url()?);
        let response = self.send(self.http.post(&url).json(body), &url).await?;
        decode(response).await
    }

    /// Transport failures are where the "is it actually there?" question gets
    /// answered, so the stale/fresh distinction is applied here rather than up
    /// front — a stale record whose daemon still answers is a non-event.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        url: &str,
    ) -> Result<reqwest::Response, DaemonError> {
        request.send().await.map_err(|err| {
            let url = url.to_string();
            let source = Box::new(err) as Box<dyn std::error::Error + Send + Sync>;
            match self.endpoint.heartbeat_is_fresh() {
                Some(false) => DaemonError::Stale { url, source },
                _ => DaemonError::Unreachable { url, source },
            }
        })
    }
}

/// Non-2xx bodies are `ApiError` JSON by contract; a body that is not is still
/// reported rather than swallowed, because "the daemon said something we did
/// not understand" is a real bug an agent should be able to relay.
async fn decode<R: DeserializeOwned>(response: reqwest::Response) -> Result<R, DaemonError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| DaemonError::Protocol(err.to_string()))?;

    if !status.is_success() {
        let message = serde_json::from_str::<ApiError>(&body)
            .map(|api| api.message)
            .unwrap_or(body);
        return Err(DaemonError::Api {
            status: status.as_u16(),
            message,
        });
    }
    serde_json::from_str(&body).map_err(|err| DaemonError::Protocol(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_handshake_tells_the_agent_to_ask_for_lore_daemon() {
        let empty = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(empty.path().to_path_buf()).unwrap();
        let err = Endpoint::DataDir(data_dir).base_url().unwrap_err();

        assert!(matches!(err, DaemonError::NotRunning(_)));
        let text = err.to_string();
        assert!(text.starts_with("the lore daemon is not running"), "{text}");
        assert!(text.contains("no daemon handshake at"), "{text}");
        assert!(text.contains("`lore daemon`"), "{text}");
        // M1: never offer to start it ourselves.
        assert!(!text.contains("starting"), "{text}");
    }

    #[test]
    fn corrupt_handshake_is_not_running_rather_than_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::write(discovery::handshake_path(&data_dir), "{ not json").unwrap();

        let err = Endpoint::DataDir(data_dir).base_url().unwrap_err();
        assert!(
            err.to_string().contains("unreadable daemon handshake"),
            "{err}"
        );
    }

    #[test]
    fn api_version_skew_is_reported_as_skew_not_as_absence() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let handshake = discovery::Handshake {
            pid: 1234,
            port: 53412,
            api_version: lore_core::API_VERSION + 1,
            daemon_version: "9.9.9".into(),
            started_at: 0,
            heartbeat_at: discovery::unix_now(),
        };
        std::fs::write(
            discovery::handshake_path(&data_dir),
            serde_json::to_string(&handshake).unwrap(),
        )
        .unwrap();

        let err = Endpoint::DataDir(data_dir).base_url().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("speaks API v2"), "{text}");
        assert!(text.contains("this MCP proxy speaks v1"), "{text}");
    }

    #[test]
    fn a_fresh_handshake_yields_the_loopback_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let handshake = discovery::Handshake {
            pid: 1234,
            port: 53412,
            api_version: lore_core::API_VERSION,
            daemon_version: "0.1.0".into(),
            started_at: 0,
            heartbeat_at: discovery::unix_now(),
        };
        std::fs::write(
            discovery::handshake_path(&data_dir),
            serde_json::to_string(&handshake).unwrap(),
        )
        .unwrap();

        let endpoint = Endpoint::DataDir(data_dir);
        assert_eq!(endpoint.base_url().unwrap(), "http://127.0.0.1:53412/v1");
        assert_eq!(endpoint.heartbeat_is_fresh(), Some(true));
    }

    #[test]
    fn a_long_dead_heartbeat_reads_as_stale_without_vetoing_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let handshake = discovery::Handshake {
            pid: 1234,
            port: 53412,
            api_version: lore_core::API_VERSION,
            daemon_version: "0.1.0".into(),
            started_at: 0,
            heartbeat_at: discovery::unix_now() - 3600,
        };
        std::fs::write(
            discovery::handshake_path(&data_dir),
            serde_json::to_string(&handshake).unwrap(),
        )
        .unwrap();

        let endpoint = Endpoint::DataDir(data_dir);
        // Still resolves: liveness is decided by the request, not the clock.
        assert!(endpoint.base_url().is_ok());
        assert_eq!(endpoint.heartbeat_is_fresh(), Some(false));
    }
}
