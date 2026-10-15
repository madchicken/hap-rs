use futures::{
    channel::oneshot,
    future::{self, BoxFuture, Future, FutureExt, TryFutureExt},
    lock::Mutex,
};
use hyper::{Body, Method, Request, Response, StatusCode, server::conn::Http, service::Service};
use log::{debug, error, info};
use std::{
    error::Error as StdError,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::net::TcpListener;

use crate::{
    Error, Result,
    event::Event,
    pointer,
    server::ResourceRequest,
    transport::{
        http::{
            EventObject, event_response,
            handler::{
                HandlerExt, JsonHandler, TlvHandler,
                accessories::Accessories,
                characteristics::{GetCharacteristics, UpdateCharacteristics},
                identify::Identify,
                pair_setup::PairSetup,
                pair_verify::PairVerify,
                pairings::Pairings,
            },
            status_response,
        },
        tcp::{EncryptedStream, Session, StreamWrapper},
    },
};

struct Handlers {
    pub pair_setup: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub pair_verify: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub accessories: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub get_characteristics: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub put_characteristics: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub pairings: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
    pub identify: Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>,
}

struct Api {
    controller_id: pointer::ControllerId,
    event_subscriptions: pointer::EventSubscriptions,
    config: pointer::Config,
    storage: pointer::Storage,
    accessory_database: pointer::AccessoryDatabase,
    event_emitter: pointer::EventEmitter,
    resource_handler: pointer::ResourceHandler,
    handlers: Handlers,
}

impl Api {
    fn new(
        controller_id: pointer::ControllerId,
        event_subscriptions: pointer::EventSubscriptions,
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
        resource_handler: pointer::ResourceHandler,
        session_sender: oneshot::Sender<Session>,
    ) -> Self {
        Api {
            controller_id,
            event_subscriptions,
            config,
            storage,
            accessory_database,
            event_emitter,
            resource_handler,
            handlers: Handlers {
                pair_setup: Arc::new(Mutex::new(Box::new(TlvHandler::from(PairSetup::new())))),
                pair_verify: Arc::new(Mutex::new(Box::new(TlvHandler::from(PairVerify::new(session_sender))))),
                accessories: Arc::new(Mutex::new(Box::new(JsonHandler::from(Accessories::new())))),
                get_characteristics: Arc::new(Mutex::new(Box::new(JsonHandler::from(GetCharacteristics::new())))),
                put_characteristics: Arc::new(Mutex::new(Box::new(JsonHandler::from(UpdateCharacteristics::new())))),
                pairings: Arc::new(Mutex::new(Box::new(TlvHandler::from(Pairings::new())))),
                identify: Arc::new(Mutex::new(Box::new(JsonHandler::from(Identify::new())))),
            },
        }
    }
}

impl Service<Request<Body>> for Api {
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = std::result::Result<Self::Response, Self::Error>> + Send>>;
    type Response = Response<Body>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let (parts, body) = req.into_parts();
        let method = parts.method;
        let uri = parts.uri;

        let method_str = method.to_string();
        let path = uri.path().to_string();
        info!("HAP request received: {} {}", method_str, path);

        if method == Method::POST && path == "/resource" {
            return handle_resource(self.resource_handler.clone(), body).boxed();
        }

        let mut handler: Option<Arc<Mutex<Box<dyn HandlerExt + Send + Sync>>>> = match (method, uri.path()) {
            (Method::POST, "/pair-setup") => Some(self.handlers.pair_setup.clone()),
            (Method::POST, "/pair-verify") => Some(self.handlers.pair_verify.clone()),
            (Method::GET, "/accessories") => Some(self.handlers.accessories.clone()),
            (Method::GET, "/characteristics") => Some(self.handlers.get_characteristics.clone()),
            (Method::PUT, "/characteristics") => Some(self.handlers.put_characteristics.clone()),
            (Method::POST, "/pairings") => Some(self.handlers.pairings.clone()),
            (Method::POST, "/identify") => Some(self.handlers.identify.clone()),
            _ => None,
        };

        let controller_id = self.controller_id.clone();
        let event_subscriptions = self.event_subscriptions.clone();
        let config = self.config.clone();
        let storage = self.storage.clone();
        let accessory_database = self.accessory_database.clone();
        let event_emitter = self.event_emitter.clone();

        let fut = async move {
            let result = match handler.take() {
                Some(handler) => {
                    handler
                        .lock()
                        .await
                        .handle(
                            uri,
                            body,
                            controller_id,
                            event_subscriptions,
                            config,
                            storage,
                            accessory_database,
                            event_emitter,
                        )
                        .await
                },
                None => future::ready(status_response(StatusCode::NOT_FOUND)).await,
            };
            match &result {
                Ok(resp) => info!("HAP request completed: {} {} -> {}", method_str, path, resp.status()),
                Err(e) => error!("HAP request failed: {} {} -> {:?}", method_str, path, e),
            }
            result
        }
        .boxed();

        fut
    }
}

/// Answers `POST /resource` (camera snapshots) through the handler registered
/// with [`IpServer::set_resource_handler`](crate::server::IpServer::set_resource_handler).
async fn handle_resource(handler: pointer::ResourceHandler, body: Body) -> Result<Response<Body>> {
    let Some(handler) = handler.read().unwrap_or_else(|e| e.into_inner()).clone() else {
        return status_response(StatusCode::NOT_FOUND);
    };
    let body = hyper::body::to_bytes(body).await?;
    let request: ResourceRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => {
            error!("invalid /resource request: {e}");
            return status_response(StatusCode::BAD_REQUEST);
        },
    };
    if request.resource_type != "image" {
        return status_response(StatusCode::BAD_REQUEST);
    }
    match handler(request).await {
        Ok(image) => Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "image/jpeg")
            .body(Body::from(image))
            .map_err(Error::from),
        Err(e) => {
            error!("snapshot failed: {e}");
            status_response(StatusCode::INTERNAL_SERVER_ERROR)
        },
    }
}

#[derive(Clone)]
pub struct Server {
    config: pointer::Config,
    storage: pointer::Storage,
    accessory_database: pointer::AccessoryDatabase,
    event_emitter: pointer::EventEmitter,
    mdns_responder: pointer::MdnsResponder,
    resource_handler: pointer::ResourceHandler,
}

impl Server {
    pub fn new(
        config: pointer::Config,
        storage: pointer::Storage,
        accessory_database: pointer::AccessoryDatabase,
        event_emitter: pointer::EventEmitter,
        mdns_responder: pointer::MdnsResponder,
        resource_handler: pointer::ResourceHandler,
    ) -> Self {
        Server {
            config,
            storage,
            accessory_database,
            event_emitter,
            mdns_responder,
            resource_handler,
        }
    }

    pub fn run_handle(&self) -> BoxFuture<'_, Result<()>> {
        let config = self.config.clone();
        let storage = self.storage.clone();
        let accessory_database = self.accessory_database.clone();
        let event_emitter = self.event_emitter.clone();
        let mdns_responder = self.mdns_responder.clone();

        async move {
            let config_lock = config.lock().await;
            let socket_addr = SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), config_lock.port);

            info!("binding TCP listener on {}", &socket_addr);
            let listener = TcpListener::bind(socket_addr).await?;

            mdns_responder.update_records(config_lock.clone()).await;
            drop(config_lock);

            loop {
                let (stream, _socket_addr) = listener.accept().await?;

                debug!("incoming TCP stream from {}", stream.peer_addr()?);

                let (
                    encrypted_stream,
                    stream_incoming,
                    stream_outgoing,
                    session_sender,
                    incoming_waker,
                    outgoing_waker,
                ) = EncryptedStream::new(stream);
                let stream_wrapper =
                    StreamWrapper::new(stream_incoming, stream_outgoing.clone(), incoming_waker, outgoing_waker);
                let event_subscriptions = Arc::new(Mutex::new(vec![]));

                let api = Api::new(
                    encrypted_stream.controller_id.clone(),
                    event_subscriptions.clone(),
                    self.config.clone(),
                    storage.clone(),
                    accessory_database.clone(),
                    event_emitter.clone(),
                    self.resource_handler.clone(),
                    session_sender,
                );

                event_emitter.lock().await.add_listener(Box::new(move |event| {
                    let event_subscriptions_ = event_subscriptions.clone();
                    let stream_outgoing_ = stream_outgoing.clone();
                    async move {
                        if let Event::CharacteristicValueChanged { aid, iid, ref value } = *event {
                            let mut dropped_subscriptions = vec![];
                            for (i, &(s_aid, s_iid)) in event_subscriptions_.lock().await.iter().enumerate() {
                                if s_aid == aid && s_iid == iid {
                                    let event = EventObject {
                                        aid,
                                        iid,
                                        value: value.clone(),
                                    };
                                    let event_res =
                                        event_response(vec![event]).expect("couldn't create event response");
                                    if stream_outgoing_.unbounded_send(event_res).is_err() {
                                        dropped_subscriptions.push(i);
                                    }
                                }
                            }
                            let mut ev = event_subscriptions_.lock().await;
                            for s in dropped_subscriptions {
                                ev.remove(s);
                            }
                        }
                    }
                    .boxed()
                }));

                let mut http = Http::new();
                http.http1_only(true);
                http.http1_half_close(true);
                http.http1_keep_alive(true);
                http.http1_preserve_header_case(true);

                tokio::spawn(
                    encrypted_stream
                        .map_err(|e| {
                            if e.kind() == std::io::ErrorKind::BrokenPipe
                                || e.kind() == std::io::ErrorKind::ConnectionReset
                            {
                                debug!("HomeKit controller closed connection: {:?}", e);
                            } else {
                                error!("{:?}", e);
                            }
                        })
                        .map(|_| ()),
                );
                tokio::spawn(
                    http.serve_connection(stream_wrapper, api)
                        .map_err(|e| {
                            let is_broken_pipe = StdError::source(&e)
                                .and_then(|s| s.downcast_ref::<std::io::Error>())
                                .map(|io| {
                                    io.kind() == std::io::ErrorKind::BrokenPipe
                                        || io.kind() == std::io::ErrorKind::ConnectionReset
                                })
                                .unwrap_or(false);
                            if is_broken_pipe {
                                debug!("HomeKit controller closed connection: {:?}", e);
                            } else {
                                error!("{:?}", e);
                            }
                        })
                        .map(|_| ()),
                );
            }

            #[allow(unreachable_code)]
            Ok(())
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use super::*;

    fn request(kind: &str) -> Body {
        Body::from(format!(
            r#"{{"aid":2,"resource-type":"{kind}","image-width":640,"image-height":360}}"#
        ))
    }

    fn slot(handler: Option<crate::server::ResourceHandler>) -> pointer::ResourceHandler {
        Arc::new(RwLock::new(handler))
    }

    #[tokio::test]
    async fn resource_without_handler_is_not_found() {
        let resp = handle_resource(slot(None), request("image")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resource_returns_the_snapshot() {
        let handler: crate::server::ResourceHandler = Arc::new(|req: ResourceRequest| {
            async move {
                assert_eq!(req.aid, Some(2));
                assert_eq!((req.image_width, req.image_height), (640, 360));
                Ok(vec![0xff, 0xd8, 0xff])
            }
            .boxed()
        });
        let handler = slot(Some(handler));

        let resp = handle_resource(handler.clone(), request("image")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[hyper::header::CONTENT_TYPE], "image/jpeg");
        let body = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], &[0xff, 0xd8, 0xff]);

        let resp = handle_resource(handler, request("video")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn resource_handler_failure_is_a_server_error() {
        let handler: crate::server::ResourceHandler =
            Arc::new(|_: ResourceRequest| async move { Err("camera offline".into()) }.boxed());
        let resp = handle_resource(slot(Some(handler)), request("image")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
