use log::info;
use mdns_sd::{IfKind, Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::task::JoinHandle;

use crate::pointer;

/// An mDNS Responder. Used to announce the Accessory's name and HAP TXT records to potential controllers.
pub struct MdnsResponder {
    mdns: ServiceDaemon,
    receiver: Receiver<ServiceEvent>,
    service_info: ServiceInfo,
    service_fullname: String,
}

impl MdnsResponder {
    /// Creates a new mDNS Responder.
    pub async fn new(config: pointer::Config) -> Self {
        let mdns = ServiceDaemon::new().expect("Failed to create daemon");
        mdns.disable_interface(IfKind::IPv6).unwrap();
        let receiver = mdns.browse("_hap._tcp.local.").unwrap();

        let config = config.lock().await;
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
        drop(config);

        let service_info = ServiceInfo::new("_hap._tcp.local.", &instance_name, &host_name, "", port, tr.as_slice())
            .expect("valid service info")
            .enable_addr_auto();
        let service_fullname = service_info.get_fullname().to_string();

        MdnsResponder {
            mdns,
            receiver,
            service_info,
            service_fullname,
        }
    }

    /// Derives new mDNS TXT records from the server's `Config`.
    pub async fn update_records(&self) {
        info!("Unsetting mDNS records");
        let _ = self.mdns.unregister(&self.service_fullname);
        info!("Attempting to set a new mDNS records");
        self.mdns
            .register(self.service_info.clone())
            .expect("Failed to register mDNS service");

        info!(
            "Successfully set mDNS records: {:?}",
            self.service_info.get_properties()
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
        self.mdns.unregister(&self.service_fullname)
    }
}

impl Drop for MdnsResponder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
