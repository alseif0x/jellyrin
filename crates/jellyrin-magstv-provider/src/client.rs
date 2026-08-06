//! Typed portal acquisition flow built on the verified MAGSTV codec.

use serde::de::DeserializeOwned;
use std::{collections::HashSet, env, fmt};
use time::OffsetDateTime;

use crate::{
    MagstvAssetData, MagstvCategory, MagstvColumnContentsData, MagstvConfig, MagstvConnector,
    MagstvGetAuthInfoData, MagstvGetAuthInfoRequest, MagstvGetColumnContentsRequest,
    MagstvGetItemDataData, MagstvGetItemDataRequest, MagstvGetLiveDataRequest,
    MagstvGetShelveRequest, MagstvLiveData, MagstvLiveTvImport, MagstvLoginData,
    MagstvLoginRequest, MagstvMediaImport, MagstvMediaKind, MagstvPortalCodec,
    MagstvPortalIdentity, MagstvPortalResponse, MagstvProviderError, MagstvSecret, MagstvSession,
    MagstvShelveData, MagstvSlbInfoData, MagstvSlbInfoRequest, MagstvStartPlayVodData,
    MagstvStartPlayVodRequest, MagstvTransport, PortalOperation, PortalRequest, PortalResponse,
};

const MAX_LIVE_CATEGORIES: usize = 128;
const DEFAULT_MAGSTV_MAC_ADDR: &str = "02:00:00:00:00:01";

/// Session material returned by a successful portal login. It is intentionally
/// separate from the persisted config and has a redacted Debug implementation.
#[derive(Clone, PartialEq, Eq)]
pub struct MagstvAuthenticatedSession {
    identity: MagstvPortalIdentity,
    session: MagstvSession,
}

impl fmt::Debug for MagstvAuthenticatedSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvAuthenticatedSession")
            .field("identity", &self.identity)
            .field("session", &self.session)
            .finish()
    }
}

impl MagstvAuthenticatedSession {
    pub fn identity(&self) -> &MagstvPortalIdentity {
        &self.identity
    }

    pub fn session(&self) -> &MagstvSession {
        &self.session
    }
}

/// High-level login and catalogue client. The transport is generic so unit
/// tests can exercise the complete flow without contacting the service.
pub struct MagstvPortalClient<T> {
    config: MagstvConfig,
    connector: MagstvConnector<T, MagstvPortalCodec>,
}

impl<T> fmt::Debug for MagstvPortalClient<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MagstvPortalClient")
            .field("config", &self.config)
            .field("connector", &"[CONFIGURED]")
            .finish()
    }
}

impl<T> MagstvPortalClient<T>
where
    T: MagstvTransport,
{
    pub fn new(
        config: MagstvConfig,
        transport: T,
        codec: MagstvPortalCodec,
    ) -> Result<Self, MagstvProviderError> {
        config.validates_for_connection()?;
        Ok(Self {
            config,
            connector: MagstvConnector::new(transport, codec),
        })
    }

    pub fn config(&self) -> &MagstvConfig {
        &self.config
    }

    pub async fn authenticate(
        &self,
        secret: &MagstvSecret,
    ) -> Result<MagstvAuthenticatedSession, MagstvProviderError> {
        self.authenticate_at(secret, OffsetDateTime::now_utc())
            .await
    }

    pub async fn authenticate_at(
        &self,
        secret: &MagstvSecret,
        now: OffsetDateTime,
    ) -> Result<MagstvAuthenticatedSession, MagstvProviderError> {
        // The Android client always supplies a non-null Ethernet identifier.
        // Keep it stable and hidden from the UI; operators may preserve an
        // existing authorised device identity through the runtime variable.
        let mac_addr = env::var("MAGSTV_MAC_ADDR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MAGSTV_MAC_ADDR.to_string());
        let login = MagstvLoginRequest::from_secret(secret, mac_addr)?;
        let arguments =
            serde_json::to_value(login).map_err(|_| MagstvProviderError::InvalidPortalPayload)?;
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(PortalOperation::Authenticate, arguments),
                None,
                now,
            )
            .await?;
        let login: MagstvPortalResponse<MagstvLoginData> = decode_response(response)?;
        let data = successful_data(login)?;
        let identity = data.identity()?;
        let session = MagstvSession::new(identity.user_token().to_string(), now);
        Ok(MagstvAuthenticatedSession { identity, session })
    }

    pub async fn get_column_contents(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvGetColumnContentsRequest,
    ) -> Result<MagstvColumnContentsData, MagstvProviderError> {
        self.get_column_contents_at(authenticated, request, OffsetDateTime::now_utc())
            .await
    }

    pub async fn get_column_contents_at(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvGetColumnContentsRequest,
        now: OffsetDateTime,
    ) -> Result<MagstvColumnContentsData, MagstvProviderError> {
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    PortalOperation::ListLiveCategories,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                now,
            )
            .await?;
        successful_data(decode_response(response)?)
    }

    pub async fn get_live_data(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvGetLiveDataRequest,
    ) -> Result<MagstvLiveData, MagstvProviderError> {
        self.get_live_data_at(authenticated, request, OffsetDateTime::now_utc())
            .await
    }

    pub async fn get_live_data_at(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvGetLiveDataRequest,
        now: OffsetDateTime,
    ) -> Result<MagstvLiveData, MagstvProviderError> {
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    PortalOperation::ListLiveChannels,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                now,
            )
            .await?;
        successful_data(decode_response(response)?)
    }

    pub async fn get_shelve_data(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        operation: PortalOperation,
        request: MagstvGetShelveRequest,
    ) -> Result<MagstvShelveData, MagstvProviderError> {
        if !matches!(
            operation,
            PortalOperation::ListMovies | PortalOperation::ListSeries
        ) {
            return Err(MagstvProviderError::OperationUnverified { operation });
        }
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    operation,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                OffsetDateTime::now_utc(),
            )
            .await?;
        successful_data(decode_response(response)?)
    }

    pub async fn get_item_data(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvGetItemDataRequest,
    ) -> Result<MagstvAssetData, MagstvProviderError> {
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    PortalOperation::ListEpisodes,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                OffsetDateTime::now_utc(),
            )
            .await?;
        let data: MagstvGetItemDataData = successful_data(decode_response(response)?)?;
        data.asset_data
            .ok_or(MagstvProviderError::InvalidPortalPayload)
    }

    pub async fn get_auth_info(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvGetAuthInfoRequest,
    ) -> Result<MagstvGetAuthInfoData, MagstvProviderError> {
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    PortalOperation::GetAuthInfo,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                OffsetDateTime::now_utc(),
            )
            .await?;
        successful_data(decode_response(response)?)
    }

    pub async fn get_slb_info(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvSlbInfoRequest,
    ) -> Result<MagstvSlbInfoData, MagstvProviderError> {
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    PortalOperation::GetSlbInfo,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                OffsetDateTime::now_utc(),
            )
            .await?;
        successful_data(decode_response(response)?)
    }

    pub async fn start_play_vod(
        &self,
        authenticated: &MagstvAuthenticatedSession,
        request: MagstvStartPlayVodRequest,
    ) -> Result<MagstvStartPlayVodData, MagstvProviderError> {
        let response = self
            .connector
            .execute_at(
                &self.config,
                &PortalRequest::new(
                    PortalOperation::ResolveVodPlayback,
                    serde_json::to_value(request)
                        .map_err(|_| MagstvProviderError::InvalidPortalPayload)?,
                ),
                Some(authenticated.session()),
                OffsetDateTime::now_utc(),
            )
            .await?;
        successful_data(decode_response(response)?)
    }

    /// Loads the movie and series cards exposed by the authenticated portal.
    /// No playback URL or session token is copied into the returned catalog.
    pub async fn import_media(
        &self,
        secret: &MagstvSecret,
        page_size: i32,
        item_limit: usize,
    ) -> Result<MagstvMediaImport, MagstvProviderError> {
        let authenticated = self.authenticate(secret).await?;
        let roots = self
            .get_column_contents(
                &authenticated,
                authenticated
                    .identity()
                    .column_contents_request(None, 1, Some(100)),
            )
            .await?;
        let page_size = page_size.clamp(1, 500);
        let item_limit = item_limit.clamp(1, 10_000);
        let mut import = MagstvMediaImport::default();

        for (root_code, operation, kind) in [
            (
                "masnew_movies",
                PortalOperation::ListMovies,
                MagstvMediaKind::Movie,
            ),
            (
                "masnew_series",
                PortalOperation::ListSeries,
                MagstvMediaKind::Series,
            ),
        ] {
            let Some(root_id) = roots
                .child_column_list
                .iter()
                .find(|column| column.code.as_deref() == Some(root_code))
                .and_then(|column| column.id)
            else {
                continue;
            };
            let columns = self
                .get_column_contents(
                    &authenticated,
                    authenticated
                        .identity()
                        .column_contents_request(Some(root_id), 1, Some(100)),
                )
                .await?;
            let mut seen = HashSet::new();
            let destination = match kind {
                MagstvMediaKind::Movie => &mut import.movies,
                MagstvMediaKind::Series => &mut import.series,
            };
            for column in columns.child_column_list {
                let Some(column_id) = column.id else { continue };
                let mut page_num = 1;
                loop {
                    let shelf = self
                        .get_shelve_data(
                            &authenticated,
                            operation,
                            authenticated
                                .identity()
                                .shelve_request(column_id, "2", page_num, page_size),
                        )
                        .await?;
                    let returned = shelf.asset_list.len();
                    for asset in shelf.asset_list {
                        let request_type = asset.r#type.clone();
                        if let Some(mut item) = asset.into_media_item(kind)
                            && seen.insert(item.id.clone())
                        {
                            item.column_id = Some(column_id);
                            item.request_type = request_type;
                            destination.push(item);
                            if destination.len() >= item_limit {
                                break;
                            }
                        }
                    }
                    if destination.len() >= item_limit || returned < page_size as usize {
                        break;
                    }
                    page_num += 1;
                }
                if destination.len() >= item_limit {
                    break;
                }
            }
        }

        // Series shelves contain season cards. The detail endpoint exposes
        // the actual `simpleProgramList`; turn that into Episode rows so
        // Jellyfin can build its normal Series -> Season -> Episode tree.
        // Stop on the first rejected detail call: the card catalogue remains
        // useful and a refresh must not turn a portal capability mismatch
        // into thousands of identical requests.
        let episode_limit = item_limit.saturating_mul(20).clamp(item_limit, 100_000);
        let language = env::var("MAGSTV_APP_LANGUAGE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "en".to_string());
        let mac_addr = env::var("MAGSTV_MAC_ADDR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MAGSTV_MAC_ADDR.to_string());
        let series_snapshot = import.series.clone();
        let mut seen_episode_ids = HashSet::new();
        'series: for series in series_snapshot {
            if import.episodes.len() >= episode_limit {
                break;
            }
            let (Some(column_id), Some(request_type)) =
                (series.column_id, series.request_type.clone())
            else {
                continue;
            };
            let first_season = season_number_from_name(&series.name);
            let first_detail = match self
                .get_item_data(
                    &authenticated,
                    authenticated.identity().item_data_request(
                        series.id.clone(),
                        request_type.clone(),
                        Some("0".to_string()),
                        Some(language.clone()),
                        mac_addr.clone(),
                    ),
                )
                .await
            {
                Ok(detail) => detail,
                Err(_) => break,
            };

            let mut season_requests = vec![(series.id.clone(), first_season)];
            for season in &first_detail.same_season_series_list {
                let Some(content_id) = season
                    .content_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                if season_requests
                    .iter()
                    .any(|(existing, _)| existing == content_id)
                {
                    continue;
                }
                season_requests.push((content_id.to_string(), season.season_number));
            }

            for (index, (season_content_id, season_number)) in
                season_requests.into_iter().enumerate()
            {
                let detail = if index == 0 {
                    first_detail.clone()
                } else {
                    match self
                        .get_item_data(
                            &authenticated,
                            authenticated.identity().item_data_request(
                                season_content_id.clone(),
                                request_type.clone(),
                                Some("0".to_string()),
                                Some(language.clone()),
                                mac_addr.clone(),
                            ),
                        )
                        .await
                    {
                        Ok(detail) => detail,
                        Err(_) => break 'series,
                    }
                };
                for episode in detail.into_episode_items(
                    series.id.clone(),
                    series.name.clone(),
                    season_number,
                    Some(column_id),
                    Some(request_type.clone()),
                ) {
                    if seen_episode_ids.insert(episode.id.clone()) {
                        import.episodes.push(episode);
                        if import.episodes.len() >= episode_limit {
                            break 'series;
                        }
                    }
                }
            }
        }
        Ok(import)
    }

    /// Logs in, obtains the portal's live columns, and loads each live column
    /// into the safe Jellyrin catalogue shape. Only catalog identities leave
    /// this method; play addresses stay in the short-lived response object.
    pub async fn import_live_tv(
        &self,
        secret: &MagstvSecret,
        root_column_id: Option<i32>,
        page_size: i32,
    ) -> Result<MagstvLiveTvImport, MagstvProviderError> {
        let now = OffsetDateTime::now_utc();
        let authenticated = self.authenticate_at(secret, now).await?;
        let page_size = page_size.clamp(1, 500);
        let live_root_id = if let Some(root_column_id) = root_column_id {
            root_column_id
        } else {
            let roots = self
                .get_column_contents_at(
                    &authenticated,
                    authenticated
                        .identity()
                        .column_contents_request(None, 1, Some(100)),
                    now,
                )
                .await?;
            roots
                .child_column_list
                .into_iter()
                .find(|column| {
                    column.code.as_deref() == Some("masnew_live")
                        || column.r#type.as_deref() == Some("1")
                })
                .and_then(|column| column.id)
                .ok_or(MagstvProviderError::InvalidPortalPayload)?
        };
        let columns = self
            .get_column_contents_at(
                &authenticated,
                authenticated.identity().column_contents_request(
                    Some(live_root_id),
                    1,
                    Some(page_size),
                ),
                now,
            )
            .await?;

        let mut categories = Vec::new();
        let mut channels = Vec::new();
        for child in columns
            .child_column_list
            .into_iter()
            .take(MAX_LIVE_CATEGORIES)
        {
            let Some(category) = child.clone().into_catalog_category() else {
                continue;
            };
            let Some(column_id) = child.id else {
                continue;
            };
            let data = self
                .get_live_data_at(
                    &authenticated,
                    authenticated
                        .identity()
                        .live_data_request(column_id, Some(1), Some(page_size)),
                    now,
                )
                .await?;
            categories.push(category.clone());
            channels.extend(
                data.channel_list
                    .into_iter()
                    .filter_map(|channel| channel.into_catalog_channel(category.id.clone())),
            );
        }

        // Some portal revisions expose a live column directly and return no
        // children. Keep that shape useful without inventing a category list.
        if categories.is_empty() {
            if let Some(column_id) = root_column_id {
                let data = self
                    .get_live_data_at(
                        &authenticated,
                        authenticated.identity().live_data_request(
                            column_id,
                            Some(1),
                            Some(page_size),
                        ),
                        now,
                    )
                    .await?;
                let category = MagstvCategory {
                    id: column_id.to_string(),
                    name: "Live TV".to_string(),
                };
                channels.extend(
                    data.channel_list
                        .into_iter()
                        .filter_map(|channel| channel.into_catalog_channel(category.id.clone())),
                );
                categories.push(category);
            }
        }

        // The portal intentionally repeats popular channels in several live
        // columns. Jellyrin stores one remote channel per tuner, so retain the
        // first category assignment and discard later duplicates.
        let mut seen_channel_ids = HashSet::new();
        channels.retain(|channel| seen_channel_ids.insert(channel.id.clone()));
        let visible_category_ids = channels
            .iter()
            .map(|channel| channel.category_id.as_str())
            .collect::<HashSet<_>>();
        categories.retain(|category| visible_category_ids.contains(category.id.as_str()));

        Ok(MagstvLiveTvImport {
            categories,
            channels,
        })
    }
}

fn decode_response<T: DeserializeOwned>(
    response: PortalResponse,
) -> Result<T, MagstvProviderError> {
    let payload_shape = response.payload.as_object().map(|object| {
        let keys = object.keys().cloned().collect::<Vec<_>>();
        let data_type = object
            .get("data")
            .map(|value| match value {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::String(_) => "string",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::Object(_) => "object",
            })
            .unwrap_or("missing");
        (keys, data_type)
    });
    serde_json::from_value(response.payload).map_err(|_| {
        if let Some((keys, data_type)) = payload_shape {
            tracing::warn!(
                ?keys,
                data_type,
                "MAGSTV portal response shape did not match DTO"
            );
            MagstvProviderError::UnexpectedPortalDataType { data_type }
        } else {
            tracing::warn!("MAGSTV portal response was not a JSON object");
            MagstvProviderError::InvalidPortalPayload
        }
    })
}

fn successful_data<T>(response: MagstvPortalResponse<T>) -> Result<T, MagstvProviderError> {
    let unsuccessful_code = response
        .return_code
        .as_deref()
        .filter(|code| !matches!(code.trim(), "0" | "200"));
    if let Some(return_code) = unsuccessful_code {
        tracing::warn!(
            return_code,
            data_present = response.data.is_some(),
            error_message_len = response.error_message.as_deref().map(str::len).unwrap_or(0),
            "MAGSTV portal returned a non-success code"
        );
        let return_code = return_code.trim();
        let return_code = if return_code.len() <= 64
            && return_code
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return_code.to_string()
        } else {
            "invalid".to_string()
        };
        return Err(MagstvProviderError::PortalRejected { return_code });
    }
    response.data.ok_or_else(|| {
        tracing::warn!(
            return_code = response.return_code.as_deref().unwrap_or("missing"),
            "MAGSTV portal response did not contain data"
        );
        MagstvProviderError::InvalidPortalPayload
    })
}

fn season_number_from_name(name: &str) -> Option<i32> {
    let last = name
        .split_whitespace()
        .last()?
        .trim_matches(|character: char| !character.is_ascii_alphanumeric());
    let digits = last
        .strip_prefix('T')
        .or_else(|| last.strip_prefix('t'))
        .or_else(|| last.strip_prefix('S'))
        .or_else(|| last.strip_prefix('s'))?;
    digits.parse::<i32>().ok().filter(|number| *number > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        MagstvCommonParams, MagstvPortalCodec, MagstvPortalKey, PortalTransportResponse,
        VerifiedWireRequest,
    };
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    struct FixtureTransport {
        codec: MagstvPortalCodec,
        calls: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl MagstvTransport for FixtureTransport {
        async fn exchange(
            &self,
            _bootstrap_url: &str,
            request: &VerifiedWireRequest,
        ) -> Result<PortalTransportResponse, MagstvProviderError> {
            self.calls
                .lock()
                .unwrap()
                .push(request.relative_path().to_string());
            let plaintext = self
                .codec
                .decode_wire_for_test(request.body())
                .expect("fixture request decrypts");
            let body = if request.relative_path().ends_with("/login") {
                json!({
                    "returnCode": "0",
                    "data": {"userId": "fixture-user", "userToken": "fixture-token"}
                })
            } else if request.relative_path().ends_with("getColumnContents") {
                let child_columns = if plaintext.get("columnId").is_none() {
                    json!([{"id": 1, "code": "masnew_live", "name": "Live", "type": "1"}])
                } else {
                    json!([{"id": 7, "code": "news", "name": "News"}])
                };
                json!({"returnCode": "0", "data": {"childColumnList": child_columns}})
            } else {
                let column_id = plaintext["columnId"].clone();
                let inner = json!({"channelList": [{
                    "channelCode": format!("channel-{}", column_id),
                    "name": "Fixture Channel"
                }]});
                json!({"returnCode": "0", "data": inner})
            };
            Ok(PortalTransportResponse {
                status: 200,
                content_type: Some("application/json".to_string()),
                body: serde_json::to_vec(&body).unwrap(),
            })
        }
    }

    fn common() -> MagstvCommonParams {
        MagstvCommonParams {
            login_type: "1".to_string(),
            app_language: "es".to_string(),
            apk_version: "49905".to_string(),
            sys_version: "system".to_string(),
            app_id: "app".to_string(),
            hardware_info: "hardware".to_string(),
            model: "model".to_string(),
            product: "product".to_string(),
            cpu: "x86_64".to_string(),
            b29: "b29".to_string(),
            reserve1: "reserve".to_string(),
            portal_code: "live".to_string(),
            device_token: "device".to_string(),
            sn: "sn".to_string(),
            sdk_ver: 35,
        }
    }

    #[tokio::test]
    async fn login_and_live_import_use_the_typed_flow() {
        let codec =
            MagstvPortalCodec::new(MagstvPortalKey::from_bytes([0x42; 24]), common()).unwrap();
        let transport = FixtureTransport {
            codec: codec.clone(),
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let calls = transport.calls.clone();
        let client = MagstvPortalClient::new(
            MagstvConfig {
                bootstrap_url: "https://portal.example.invalid".to_string(),
                secret_reference: "MAGSTV_TEST".to_string(),
                category_ids: Default::default(),
                excluded_category_ids: Default::default(),
                channel_limit: None,
                cdn_edge_host: None,
            },
            transport,
            codec,
        )
        .unwrap();
        let import = client
            .import_live_tv(&MagstvSecret::new("user", "password"), None, 50)
            .await
            .unwrap();
        assert_eq!(import.categories[0].id, "news");
        assert_eq!(import.channels[0].id, "channel-7");
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            [
                "/api/portalCore/v8/login",
                "/api/portalCore/v3/getColumnContents",
                "/api/portalCore/v3/getColumnContents",
                "/api/portalCore/v6/getLiveData",
            ]
        );
    }
}
