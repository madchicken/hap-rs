use futures::future::{BoxFuture, join_all};
use log::debug;
use serde_json::Value;
use std::fmt::Debug;
use uuid::Uuid;

#[derive(Debug)]
pub enum Event {
    ControllerPaired { id: Uuid },
    ControllerUnpaired { id: Uuid },
    CharacteristicValueChanged { aid: u64, iid: u64, value: Value },
}

type Listener = Box<dyn Fn(&Event) -> BoxFuture<'_, ()> + Send + Sync>;

#[derive(Default)]
pub struct EventEmitter {
    listeners: Vec<Listener>,
}

impl EventEmitter {
    pub fn new() -> EventEmitter {
        EventEmitter { listeners: vec![] }
    }

    pub fn add_listener(&mut self, listener: Listener) {
        self.listeners.push(listener);
    }

    pub async fn emit(&self, event: &Event) {
        debug!("emitting event: {:?}", event);

        join_all(self.listeners.iter().map(|listener| listener(event))).await;
    }
}
