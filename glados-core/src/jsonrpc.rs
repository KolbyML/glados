use std::{path::PathBuf, time::Duration};

use alloy_primitives::hex::FromHexError;
use entity::content;
use ethportal_api::types::enr::Enr;
use ethportal_api::types::{
    beacon::{ContentInfo as BeaconContentInfo, TraceContentInfo as BeaconTraceContentInfo},
    history::{ContentInfo as HistoryContentInfo, TraceContentInfo as HistoryTraceContentInfo},
    state::{ContentInfo as StateContentInfo, TraceContentInfo as StateTraceContentInfo},
};
use ethportal_api::utils::bytes::ByteUtilsError;
use ethportal_api::{
    BeaconNetworkApiClient, ContentKeyError, ContentValue, Discv5ApiClient,
    HistoryNetworkApiClient, NodeInfo, RoutingTableInfo, StateNetworkApiClient, Web3ApiClient,
};
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use serde_json::json;
use thiserror::Error;
use url::Url;

/// Configuration details for connection to a Portal network node.
#[derive(Clone, Debug)]
pub enum TransportConfig {
    HTTP(Url),
    IPC(PathBuf),
}

#[derive(Clone, Debug)]
pub struct PortalApi {
    pub client: HttpClient,
}

#[derive(Clone, Debug)]
pub struct PortalClient {
    pub api: PortalApi,
    pub client_info: String,
    pub enr: Enr,
}

const CONTENT_NOT_FOUND_ERROR_CODE: i32 = -39001;
#[derive(Error, Debug)]
pub enum JsonRpcError {
    #[error("received formatted response with no error, but contains a None result")]
    ContainsNone,

    #[error("received empty response (EOF only)")]
    Empty,

    #[error("HTTP client error: {0}")]
    HttpClient(String),

    /// Portal network defines "0x" as the response for absent content.
    #[error("expected special 0x 'content absent' message for content request, received HTTP response with None result")]
    SpecialMessageExpected,

    /// Portal network defines "0x" as the response for absent content.
    #[error("received special 0x 'content absent' message for non-content request, expected HTTP response with None result")]
    SpecialMessageUnexpected,

    #[error("unable to convert `{enr_string}` into ENR due to {error}")]
    InvalidEnr {
        error: String, // This source doesn't implement Error
        enr_string: String,
    },

    #[error("unable to convert {input} to hash")]
    InvalidHash { source: FromHexError, input: String },

    #[error("invalid integer conversion")]
    InvalidIntegerConversion(#[from] std::num::TryFromIntError),

    #[error("unable to convert string `{input}`")]
    InvalidJson {
        source: serde_json::Error,
        input: String,
    },

    #[error("non-specific I/O error")]
    IO(#[from] std::io::Error),

    #[error("received malformed response: {0}")]
    Malformed(serde_json::Error),

    #[error("malformed portal client URL")]
    ClientURL { url: String },

    #[error("unable to use byte utils {0}")]
    ByteUtils(#[from] ByteUtilsError),

    #[error("unable to serialize/deserialize")]
    Serialization(#[from] serde_json::Error),

    #[error("could not open file {path:?}")]
    OpenFileFailed {
        source: std::io::Error,
        path: PathBuf,
    },

    #[error("Couldn't convert bytes to ContentKey")]
    ContentKeyError(#[from] ContentKeyError),

    #[error("Query completed without finding content")]
    ContentNotFound { trace: Option<String> },
}

impl From<jsonrpsee::core::error::Error> for JsonRpcError {
    fn from(e: jsonrpsee::core::error::Error) -> Self {
        if let jsonrpsee::core::error::Error::Call(ref error) = e {
            if error.code() == CONTENT_NOT_FOUND_ERROR_CODE {
                return JsonRpcError::ContentNotFound {
                    trace: error.data().map(|data| data.to_string()),
                };
            }
        }

        // Fallback to the generic HttpClient error variant if no match
        JsonRpcError::HttpClient(e.to_string())
    }
}

pub struct Content {
    pub raw: Vec<u8>,
}

impl PortalClient {
    pub async fn from(portal_client_url: String) -> Result<Self, JsonRpcError> {
        let api = PortalApi::new(portal_client_url).await?;

        let client_info = api.get_client_version().await?;

        let node_info = api.get_node_info().await?;

        Ok(PortalClient {
            api,
            client_info,
            enr: node_info.enr,
        })
    }

    pub fn supports_trace(&self) -> bool {
        self.client_info.contains("trin") || self.client_info.contains("fluffy")
    }
}

impl PortalApi {
    pub async fn new(client_url: String) -> Result<Self, JsonRpcError> {
        let http_prefix = "http://";
        let client = if client_url.strip_prefix(http_prefix).is_some() {
            HttpClientBuilder::default()
                .request_timeout(Duration::from_secs(120))
                .build(client_url)?
        } else {
            panic!("None supported RPC interface {client_url}, use http.");
        };

        Ok(PortalApi { client })
    }

    pub async fn get_client_version(&self) -> Result<String, JsonRpcError> {
        Ok(Web3ApiClient::client_version(&self.client).await?)
    }

    pub async fn get_node_info(&self) -> Result<NodeInfo, JsonRpcError> {
        Ok(Discv5ApiClient::node_info(&self.client).await?)
    }

    pub async fn get_routing_table_info(self) -> Result<RoutingTableInfo, JsonRpcError> {
        Ok(Discv5ApiClient::routing_table_info(&self.client).await?)
    }

    pub async fn get_content(
        self,
        content: &content::Model,
    ) -> Result<Option<Content>, JsonRpcError> {
        match content.protocol_id {
            content::SubProtocol::History => {
                let result = HistoryNetworkApiClient::recursive_find_content(
                    &self.client,
                    content.content_key.clone().try_into()?,
                )
                .await?;
                let HistoryContentInfo::Content { content, .. } = result else {
                    return Err(JsonRpcError::ContentNotFound { trace: None });
                };
                Ok(Some(Content {
                    raw: content.encode(),
                }))
            }
            content::SubProtocol::State => {
                let result = StateNetworkApiClient::recursive_find_content(
                    &self.client,
                    content.content_key.clone().try_into()?,
                )
                .await?;
                let StateContentInfo::Content { content, .. } = result else {
                    return Err(JsonRpcError::ContentNotFound { trace: None });
                };
                Ok(Some(Content {
                    raw: content.encode(),
                }))
            }
            content::SubProtocol::Beacon => {
                let result = BeaconNetworkApiClient::recursive_find_content(
                    &self.client,
                    content.content_key.clone().try_into()?,
                )
                .await?;
                let BeaconContentInfo::Content { content, .. } = result else {
                    return Err(JsonRpcError::ContentNotFound { trace: None });
                };
                Ok(Some(Content {
                    raw: content.encode(),
                }))
            }
        }
    }

    pub async fn get_content_with_trace(
        self,
        content: &content::Model,
    ) -> Result<(Option<Content>, String), JsonRpcError> {
        match content.protocol_id {
            content::SubProtocol::History => {
                match HistoryNetworkApiClient::trace_recursive_find_content(
                    &self.client,
                    content.content_key.clone().try_into()?,
                )
                .await
                {
                    Ok(HistoryTraceContentInfo { content, trace, .. }) => Ok((
                        Some(Content {
                            raw: content.encode(),
                        }),
                        json!(trace).to_string(),
                    )),
                    Err(err) => match err.into() {
                        JsonRpcError::ContentNotFound { trace } => {
                            Ok((None, trace.unwrap_or_default()))
                        }
                        err => Err(err),
                    },
                }
            }
            content::SubProtocol::State => {
                match StateNetworkApiClient::trace_recursive_find_content(
                    &self.client,
                    content.content_key.clone().try_into()?,
                )
                .await
                {
                    Ok(StateTraceContentInfo { content, trace, .. }) => Ok((
                        Some(Content {
                            raw: content.encode(),
                        }),
                        json!(trace).to_string(),
                    )),
                    Err(err) => match err.into() {
                        JsonRpcError::ContentNotFound { trace } => {
                            Ok((None, trace.unwrap_or_default()))
                        }
                        err => Err(err),
                    },
                }
            }
            content::SubProtocol::Beacon => {
                match BeaconNetworkApiClient::trace_recursive_find_content(
                    &self.client,
                    content.content_key.clone().try_into()?,
                )
                .await
                {
                    Ok(BeaconTraceContentInfo { content, trace, .. }) => Ok((
                        Some(Content {
                            raw: content.encode(),
                        }),
                        json!(trace).to_string(),
                    )),
                    Err(err) => match err.into() {
                        JsonRpcError::ContentNotFound { trace } => {
                            Ok((None, trace.unwrap_or_default()))
                        }
                        err => Err(err),
                    },
                }
            }
        }
    }
}
