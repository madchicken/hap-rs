use std::sync::Mutex;

use log::info;
use mdns_sd::{IfKind, Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::task::JoinHandle;

use crate::Config;

/// An mDNS Responder. Used to announce the Accessory's name and HAP TXT records to potential controllers.
pub struct MdnsResponder {
    mdns: ServiceDaemon,
    receiver: Receiver<ServiceEvent>,
    service_info: Mutex<Option<ServiceInfo>>,
}

impl MdnsResponder {
    /// Creates a new mDNS Responder.
    pub async fn new() -> Self {
        let mdns = ServiceDaemon::new().expect("Failed to create daemon");
        mdns.disable_interface(IfKind::IPv6).unwrap();
        let receiver = mdns.browse("_hap._tcp.local.").unwrap();

        MdnsResponder {
            mdns,
            receiver,
            service_info: Mutex::new(None),
        }
    }

    fn create_service(&self, config: crate::Config) -> ServiceInfo {
        let name = config.name.replace(" ", "-");
        let port = config.port;
        let id = config.device_id.into_array()[4..6]
            .to_vec()
            .iter()
            .map(|x| format!("{:02x}", x))
            .collect::<Vec<String>>()
            .join("")
            .to_uppercase();
        let tr = config.txt_records();
        let host_name = format!("{}_{}.local.", name.to_lowercase(), id);
        let instance_name = format!("{} {}", config.name, id);

        ServiceInfo::new("_hap._tcp.local.", &instance_name, &host_name, "", port, tr.as_slice())
            .expect("valid service info")
            .enable_addr_auto()
    }

    /// Derives new mDNS TXT records from the server's `Config`.
    pub async fn update_records(&self, config: Config) {
        let mut service_info = self.service_info.lock().unwrap();
        if service_info.is_none() {
            let si = self.create_service(config);
            *service_info = Some(si);
        }
        info!("Attempting to set a new mDNS records");
        self.mdns
            .register(service_info.clone().unwrap())
            .expect("Failed to register mDNS service");

        info!(
            "Successfully set mDNS records: {:?}",
            service_info.as_ref().unwrap().get_properties()
        );
    }

    /// Returns the mDNS task to throw on a scheduler.
    pub async fn run_handle(&self) -> JoinHandle<()> {
        let receiver = self.receiver.clone();
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        info!("Found HAP service: {}", info.fullname);
                    },
                    ServiceEvent::ServiceRemoved(service_type, fullname) => {
                        info!("Removed HAP service: {}, {}", service_type, fullname);
                        break;
                    },
                    _ => {},
                }
            }
        })
    }

    /// Stops the mDNS service.
    pub fn stop(&self) -> Result<Receiver<mdns_sd::UnregisterStatus>, mdns_sd::Error> {
        let mut service_info = self.service_info.lock().unwrap();
        if let Some(service_info) = (*service_info).take() {
            self.mdns.unregister(service_info.get_fullname())
        } else {
            Err(mdns_sd::Error::Msg("No service info found".to_string()))
        }
    }
}

impl Drop for MdnsResponder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
