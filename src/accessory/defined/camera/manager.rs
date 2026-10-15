use std::{collections::HashMap, error::Error, net::IpAddr, sync::Arc};

use futures::{future::BoxFuture, lock::Mutex};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    accessory::HapAccessory,
    characteristic::{AsyncCharacteristicCallbacks, HapCharacteristic},
    pointer,
    server::Server,
    service::camera_stream_management::CameraStreamManagementService,
    tlv,
};

use super::{media::MediaProvider, protocol};

/// An accessory exposing one or more Camera RTP Stream Management services
/// (an IP camera, a video doorbell, ...) that a [`StreamManager`] can drive.
pub trait CameraStreamAccessory: HapAccessory {
    /// The stream management services, in the order used as stream index by
    /// the [`MediaProvider`].
    fn camera_streams_mut(&mut self) -> Vec<&mut CameraStreamManagementService>;
}

pub struct StreamManagerBuilder<A, MP>
where
    A: CameraStreamAccessory,
    MP: MediaProvider,
{
    accessory: A,
    provider: MP,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: Uuid,
    pub address: IpAddr,
    pub video_port: u16,
    pub audio_port: u16,
    pub video_crypto: protocol::SetupEndpointCrypto,
    pub audio_crypto: protocol::SetupEndpointCrypto,
    pub video_ssrc: u32,
    pub audio_ssrc: u32,
}

// todo: consider types to be more restricted
#[derive(Clone, Default, Debug)]
pub struct StreamOptions {
    pub video: Value,
    pub audio: Value,
    pub srtp: bool,
}

pub struct StreamManager<MP>
where
    MP: MediaProvider,
{
    accessory: pointer::Accessory,
    inner: Arc<Mutex<StreamManagerInner<MP>>>,
}

#[derive(Debug, Clone, Copy)]
enum StreamingStatus {
    Streaming,
    Available,
}

pub struct StreamManagerInner<MP>
where
    MP: MediaProvider,
{
    streaming_status: HashMap<usize, StreamingStatus>,
    sessions: HashMap<Uuid, SessionState>,
    /// Last SetupEndpoints response of each stream, returned when the
    /// controller reads the characteristic after writing its request.
    setup_responses: HashMap<usize, Vec<u8>>,
    /// Media provider
    provider: MP,
}

#[derive(Debug, Error)]
pub enum StreamManagerError {
    #[error("Invalid accessory")]
    InvalidAccessory,
    #[error("Missing characteristic")]
    MissingCharacteristic,
    #[error("Missing session")]
    SessionNotFound,
    #[error("Provider error: {0}")]
    Provider(Box<dyn Error + Send + Sync + 'static>),
    #[error(transparent)]
    Hap(#[from] crate::Error),
    #[error(transparent)]
    ProtocolError(#[from] protocol::ProtocolError),
}

type CallbackResult<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// Wires the stream management characteristics of an accessory to a
/// [`MediaProvider`] and adds the accessory to the server.
///
/// SetupEndpoints is a write-then-read exchange: the controller writes its
/// request and immediately reads back the accessory response. The response is
/// computed synchronously in the write callback and served by a read callback,
/// so it is always ready when the read arrives and the accessory database lock
/// is never taken from inside a callback.
impl<A, MP> StreamManagerBuilder<A, MP>
where
    A: CameraStreamAccessory + 'static,
    MP: MediaProvider + Send + Sync + 'static,
{
    pub fn new(accessory: A, provider: MP) -> Self { Self { accessory, provider } }

    pub async fn build<S>(self, server: &S) -> Result<StreamManager<MP>, StreamManagerError>
    where
        S: Server,
    {
        let Self {
            mut accessory,
            provider,
        } = self;

        let mgmt = Arc::new(Mutex::new(StreamManagerInner::new(provider)));
        attach(&mut accessory, &mgmt).await?;
        let accessory = server.add_accessory(accessory).await?;

        Ok(StreamManager::new(accessory, mgmt))
    }
}

/// Installs the stream management callbacks and the supported
/// configurations on every stream of `accessory`.
async fn attach<A, MP>(accessory: &mut A, mgmt: &Arc<Mutex<StreamManagerInner<MP>>>) -> Result<(), StreamManagerError>
where
    A: CameraStreamAccessory,
    MP: MediaProvider + Send + Sync + 'static,
{
    {
        for (idx, stream) in accessory.camera_streams_mut().into_iter().enumerate() {
            let m_mgmt = mgmt.clone();
            stream
                .setup_endpoint
                .on_update_async(Some(move |_: tlv::Tlv8, value: tlv::Tlv8| {
                    let mgmt = m_mgmt.clone();
                    Box::pin(async move {
                        let mut lock = mgmt.lock().await;
                        lock.setup_endpoint(idx, value).await.map_err(Box::from)
                    }) as BoxFuture<'static, CallbackResult<()>>
                }));

            let m_mgmt = mgmt.clone();
            stream.setup_endpoint.on_read_async(Some(move || {
                let mgmt = m_mgmt.clone();
                Box::pin(async move {
                    let lock = mgmt.lock().await;
                    Ok(lock.setup_responses.get(&idx).cloned().map(tlv::Tlv8))
                }) as BoxFuture<'static, CallbackResult<Option<tlv::Tlv8>>>
            }));

            let m_mgmt = mgmt.clone();
            stream
                .selected_stream_configuration
                .on_update_async(Some(move |_: tlv::Tlv8, value: tlv::Tlv8| {
                    let mgmt = m_mgmt.clone();
                    Box::pin(async move {
                        let mut lock = mgmt.lock().await;
                        lock.selected_stream_configuration(idx, value).await.map_err(Box::from)
                    }) as BoxFuture<'static, CallbackResult<()>>
                }));

            let m_mgmt = mgmt.clone();
            stream.streaming_status.on_read_async(Some(move || {
                let mgmt = m_mgmt.clone();
                Box::pin(async move {
                    let lock = mgmt.lock().await;
                    Ok(Some(lock.streaming_status(idx)))
                }) as BoxFuture<'static, CallbackResult<Option<tlv::Tlv8>>>
            }));

            // set stream configuration options and defaults
            let lock = mgmt.lock().await;
            let options = lock
                .provider
                .options(idx)
                .map_err(|err| StreamManagerError::Provider(Box::new(err)))?
                .clone();
            drop(lock);
            log::info!("settings stream options ({idx}: {:?}", options);

            stream
                .supported_video_stream_configuration
                .set_value(protocol::video_stream_config(options.video).into())
                .await?;
            stream
                .supported_audio_stream_configuration
                .set_value(protocol::audio_stream_config(options.audio).into())
                .await?;
            stream
                .supported_rtp_configuration
                .set_value(protocol::supported_rtp_config(options.srtp).into())
                .await?;
            stream
                .streaming_status
                .set_value(status_tlv(StreamingStatus::Available).0.into())
                .await?;
            if let Some(ref mut active) = stream.active {
                active.set_value(1.into()).await?;
            }
        }
    }
    Ok(())
}

fn status_tlv(status: StreamingStatus) -> tlv::Tlv8 {
    let value = match status {
        StreamingStatus::Available => protocol::STREAMING_STATUS_AVAILABLE,
        StreamingStatus::Streaming => protocol::STREAMING_STATUS_STREAMING,
    };
    tlv::Tlv8(tlv::encode(vec![(0x01u8, vec![value])]))
}

impl<MP> StreamManagerInner<MP>
where
    MP: MediaProvider,
{
    fn new(provider: MP) -> Self {
        Self {
            provider,
            sessions: Default::default(),
            streaming_status: Default::default(),
            setup_responses: Default::default(),
        }
    }

    pub async fn setup_endpoint(
        &mut self,
        idx: usize,
        value: tlv::Tlv8,
    ) -> std::result::Result<(), StreamManagerError> {
        // Serving a read stores the response as the characteristic value,
        // which runs this callback again: that is not a controller request.
        if self.setup_responses.get(&idx) == Some(&value.0) {
            return Ok(());
        }

        log::trace!("setup_endpoint({idx}): {:?}", value);
        let req = protocol::setup_endpoint_request(value.0.as_ref())?;
        log::info!("setup_endpoint({idx}): creating session {}", req.session_id);

        let res = protocol::setup_endpoint_response(
            &req,
            self.provider
                .address(idx)
                .map_err(|err| StreamManagerError::Provider(Box::new(err)))?,
        );

        self.sessions.insert(req.session_id, SessionState {
            session_id: req.session_id,
            address: req.ip,
            video_port: req.video_port,
            audio_port: req.audio_port,
            video_crypto: req.video_crypto,
            audio_crypto: req.audio_crypto,
            video_ssrc: res.video_ssrc,
            audio_ssrc: res.audio_ssrc,
        });
        self.setup_responses.insert(idx, res.tlv);

        Ok(())
    }

    pub async fn selected_stream_configuration(
        &mut self,
        idx: usize,
        value: tlv::Tlv8,
    ) -> std::result::Result<(), StreamManagerError> {
        log::debug!("selected_stream_configuration({idx}): {:?}", value);

        let req = protocol::selected_stream_configuration_request(&value.0)?;
        let session = self
            .sessions
            .get(&req.session_id)
            .ok_or(StreamManagerError::SessionNotFound)?
            .clone();
        log::info!(
            "selected_stream_configuration({idx}): {:?} {}",
            req.kind,
            req.session_id
        );

        match req.kind {
            protocol::SelectedStreamRequestKind::Start => {
                let data = protocol::stream_info_response(&value.0)?;
                self.provider.start(idx, data, &session).await.map_err(|err| {
                    self.streaming_status.insert(idx, StreamingStatus::Available);
                    StreamManagerError::Provider(Box::new(err))
                })?;
                self.streaming_status.insert(idx, StreamingStatus::Streaming);
            },
            protocol::SelectedStreamRequestKind::Stop => {
                let result = self.provider.stop(idx, &session).await;
                self.streaming_status.insert(idx, StreamingStatus::Available);
                self.sessions.remove(&req.session_id);
                self.setup_responses.remove(&idx);
                result.map_err(|err| StreamManagerError::Provider(Box::new(err)))?;
            },
            protocol::SelectedStreamRequestKind::StartAndReconfigure => {
                let data = protocol::stream_info_response(&value.0)?;
                self.provider.reconfigure(idx, data, &session).await.map_err(|err| {
                    self.streaming_status.insert(idx, StreamingStatus::Available);
                    StreamManagerError::Provider(Box::new(err))
                })?;
                self.streaming_status.insert(idx, StreamingStatus::Streaming);
            },
        }

        Ok(())
    }

    fn streaming_status(&self, idx: usize) -> tlv::Tlv8 {
        status_tlv(
            self.streaming_status
                .get(&idx)
                .copied()
                .unwrap_or(StreamingStatus::Available),
        )
    }
}

impl<MP> StreamManager<MP>
where
    MP: MediaProvider,
{
    pub fn new(accessory: pointer::Accessory, inner: Arc<Mutex<StreamManagerInner<MP>>>) -> Self {
        Self { accessory, inner }
    }

    pub fn accessory(&self) -> pointer::Accessory { self.accessory.clone() }

    pub async fn sessions(&self) -> HashMap<Uuid, SessionState> {
        let inner = self.inner.lock().await;
        inner.sessions.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, sync::Mutex as StdMutex};

    use async_trait::async_trait;
    use base64::prelude::*;
    use serde_json::json;

    use super::*;
    use crate::accessory::{AccessoryInformation, camera::VideoDoorbellAccessory};

    #[derive(Debug, Error)]
    #[error("fake provider error")]
    struct FakeError;

    #[derive(Default)]
    struct FakeProvider {
        options: StreamOptions,
        calls: Arc<StdMutex<Vec<String>>>,
    }

    const ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 10));

    #[async_trait]
    impl MediaProvider for FakeProvider {
        type Error = FakeError;

        fn address(&self, _: usize) -> Result<&IpAddr, FakeError> { Ok(&ADDRESS) }

        fn options(&self, _: usize) -> Result<&StreamOptions, FakeError> { Ok(&self.options) }

        async fn start(&self, idx: usize, _: protocol::StreamInfo, s: &SessionState) -> Result<(), FakeError> {
            self.calls.lock().unwrap().push(format!("start {idx} {}", s.session_id));
            Ok(())
        }

        async fn reconfigure(&self, _: usize, _: protocol::StreamInfo, _: &SessionState) -> Result<(), FakeError> {
            Ok(())
        }

        async fn stop(&self, idx: usize, s: &SessionState) -> Result<(), FakeError> {
            self.calls.lock().unwrap().push(format!("stop {idx} {}", s.session_id));
            Ok(())
        }
    }

    // SetupEndpoints request from the protocol tests, with the session id of
    // the SelectedRTPStreamConfiguration sample (9f4a58bc-af08-...).
    fn setup_request() -> Vec<u8> {
        let mut data = vec![
            1u8, 16, 97, 204, 48, 49, 240, 116, 75, 2, 150, 143, 224, 143, 38, 45, 40, 65, 3, 25, 1, 1, 0, 2, 12, 49,
            57, 50, 46, 49, 54, 56, 46, 50, 46, 53, 50, 3, 2, 149, 192, 4, 2, 80, 238, 4, 37, 2, 16, 8, 255, 34, 115,
            19, 121, 118, 140, 129, 31, 225, 7, 8, 113, 47, 183, 3, 14, 224, 107, 26, 188, 12, 133, 251, 85, 169, 57,
            32, 164, 163, 101, 1, 1, 0, 5, 37, 2, 16, 143, 234, 217, 0, 38, 98, 183, 222, 143, 69, 27, 46, 24, 58, 169,
            193, 3, 14, 210, 110, 127, 168, 239, 73, 194, 105, 156, 195, 164, 211, 59, 231, 1, 1, 0,
        ];
        data[2..18].copy_from_slice(&[159, 74, 88, 188, 175, 8, 79, 248, 128, 222, 231, 61, 225, 39, 125, 40]);
        data
    }

    fn selected_configuration(command: u8) -> Vec<u8> {
        let mut data = vec![
            1u8, 21, 2, 1, 1, 1, 16, 159, 74, 88, 188, 175, 8, 79, 248, 128, 222, 231, 61, 225, 39, 125, 40, 2, 52, 1,
            1, 0, 2, 9, 1, 1, 0, 2, 1, 0, 3, 1, 0, 3, 11, 1, 2, 128, 2, 2, 2, 104, 1, 3, 1, 30, 4, 23, 1, 1, 99, 2, 4,
            210, 68, 127, 99, 3, 2, 132, 0, 4, 4, 0, 0, 0, 63, 5, 2, 98, 5, 3, 44, 1, 1, 2, 2, 12, 1, 1, 1, 2, 1, 0, 3,
            1, 1, 4, 1, 30, 3, 22, 1, 1, 110, 2, 4, 82, 196, 23, 55, 3, 2, 24, 0, 4, 4, 0, 0, 160, 64, 6, 1, 13, 4, 1,
            0,
        ];
        data[4] = command;
        data
    }

    fn base64_value(bytes: &[u8]) -> Value { json!(BASE64_STANDARD.encode(bytes)) }

    #[tokio::test]
    async fn setup_endpoints_write_then_read_and_stream_lifecycle() {
        let calls = Arc::new(StdMutex::new(vec![]));
        let provider = FakeProvider {
            calls: calls.clone(),
            options: StreamOptions {
                video: json!({
                    "resolutions": [[320, 240, 15], [320, 240, 30]],
                    "codec": { "profiles": [0], "levels": [0] }
                }),
                audio: json!({ "codecs": [{ "type": "AAC-eld", "samplerate": 16 }] }),
                srtp: true,
            },
        };
        let mut doorbell = VideoDoorbellAccessory::new(1, 1, AccessoryInformation {
            name: "Door".into(),
            ..Default::default()
        })
        .unwrap();
        let mgmt = Arc::new(Mutex::new(StreamManagerInner::new(provider)));
        attach(&mut doorbell, &mgmt).await.unwrap();
        let stream = &mut doorbell.streams[0];

        // iOS writes its request as base64, then reads the response back
        HapCharacteristic::set_value(&mut stream.setup_endpoint, base64_value(&setup_request()))
            .await
            .unwrap();
        let response = HapCharacteristic::get_value(&mut stream.setup_endpoint).await.unwrap();
        let response = BASE64_STANDARD.decode(response.as_str().unwrap()).unwrap();
        let fields = tlv::decode(&response);
        assert_eq!(fields.get(&protocol::SETUP_TYPES_SESSION_ID).unwrap()[..2], [159, 74]);
        assert_eq!(fields.get(&protocol::SETUP_TYPES_STATUS).unwrap(), &vec![
            protocol::SETUP_STATUS_SUCCESS
        ]);

        // a second read serves the same response without creating a session
        let again = HapCharacteristic::get_value(&mut stream.setup_endpoint).await.unwrap();
        assert_eq!(BASE64_STANDARD.decode(again.as_str().unwrap()).unwrap(), response);
        assert_eq!(mgmt.lock().await.sessions.len(), 1);

        HapCharacteristic::set_value(
            &mut stream.selected_stream_configuration,
            base64_value(&selected_configuration(1)),
        )
        .await
        .unwrap();
        let status = HapCharacteristic::get_value(&mut stream.streaming_status)
            .await
            .unwrap();
        assert_eq!(BASE64_STANDARD.decode(status.as_str().unwrap()).unwrap(), vec![
            0x01,
            0x01,
            protocol::STREAMING_STATUS_STREAMING
        ]);

        HapCharacteristic::set_value(
            &mut stream.selected_stream_configuration,
            base64_value(&selected_configuration(0)),
        )
        .await
        .unwrap();
        assert!(mgmt.lock().await.sessions.is_empty());
        assert_eq!(*calls.lock().unwrap(), vec![
            "start 0 9f4a58bc-af08-4ff8-80de-e73de1277d28".to_string(),
            "stop 0 9f4a58bc-af08-4ff8-80de-e73de1277d28".to_string(),
        ]);
    }
}
