use crate::{
    HapType,
    Result,
    accessory::{AccessoryInformation, HapAccessory},
    service::{
        HapService,
        accessory_information::AccessoryInformationService,
        camera_stream_management::CameraStreamManagementService,
        doorbell::DoorbellService,
        microphone::MicrophoneService,
        speaker::SpeakerService,
    },
};

use serde::{
    Serialize,
    ser::{SerializeStruct, Serializer},
};

pub mod manager;
pub mod media;
pub mod protocol;

use manager::CameraStreamAccessory;

/// Hands out instance ids: the next service starts right after the highest
/// id taken by the previous one. Generated services and the accessory
/// information reserve ids for their optional characteristics, so counting
/// the characteristics would hand out ids that are already taken.
struct IidAllocator(u64);

impl IidAllocator {
    fn next_for(&mut self, service: &dyn HapService) {
        let last = service
            .get_characteristics()
            .iter()
            .map(|c| c.get_id())
            .chain(std::iter::once(service.get_id()))
            .max()
            .unwrap_or(self.0);
        self.0 = self.0.max(last + 1);
    }
}

/// Speaker for the two way audio of a video doorbell: HomeKit expects it
/// next to the microphone. Only mute and volume are exposed.
fn new_speaker(iids: &mut IidAllocator, accessory_id: u64) -> SpeakerService {
    let mut speaker = SpeakerService::new(iids.0, accessory_id);
    speaker.active = None;
    speaker.volume_control_type = None;
    speaker.volume_selector = None;
    iids.next_for(&speaker);
    speaker
}

fn find_service(services: Vec<&dyn HapService>, hap_type: HapType) -> Option<&dyn HapService> {
    services.into_iter().find(|s| s.get_type() == hap_type)
}

fn find_mut_service(services: Vec<&mut dyn HapService>, hap_type: HapType) -> Option<&mut dyn HapService> {
    services.into_iter().find(|s| s.get_type() == hap_type)
}

fn new_streams(iids: &mut IidAllocator, accessory_id: u64, count: usize) -> Vec<CameraStreamManagementService> {
    (0..count)
        .map(|_| {
            let stream = CameraStreamManagementService::new(iids.0, accessory_id);
            iids.next_for(&stream);
            stream
        })
        .collect()
}

/// Camera Accessory.
#[derive(Default, Debug)]
pub struct CameraAccessory {
    /// ID of the Camera accessory.
    id: u64,

    /// Accessory Information service.
    pub accessory_information: AccessoryInformationService,

    /// Microphone
    pub microphone: MicrophoneService,

    /// Camera streams
    pub streams: Vec<CameraStreamManagementService>,
}

impl HapAccessory for CameraAccessory {
    fn get_id(&self) -> u64 { self.id }

    fn set_id(&mut self, id: u64) { self.id = id; }

    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> { find_service(self.get_services(), hap_type) }

    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        find_mut_service(self.get_mut_services(), hap_type)
    }

    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut services: Vec<&dyn HapService> = vec![&self.accessory_information, &self.microphone];
        services.extend(self.streams.iter().map(|s| s as &dyn HapService));
        services
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut services: Vec<&mut dyn HapService> = vec![&mut self.accessory_information, &mut self.microphone];
        services.extend(self.streams.iter_mut().map(|s| s as &mut dyn HapService));
        services
    }
}

impl CameraStreamAccessory for CameraAccessory {
    fn camera_streams_mut(&mut self) -> Vec<&mut CameraStreamManagementService> { self.streams.iter_mut().collect() }
}

impl Serialize for CameraAccessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

impl CameraAccessory {
    /// Creates a new Camera Accessory.
    pub fn new(id: u64, streams_count: usize, information: AccessoryInformation) -> Result<Self> {
        let accessory_information = information.to_service(1, id)?;
        let mut iids = IidAllocator(1);
        iids.next_for(&accessory_information);

        // the microphone starts unmuted (MuteCharacteristic defaults to false)
        let microphone = MicrophoneService::new(iids.0, id);
        iids.next_for(&microphone);

        let streams = new_streams(&mut iids, id, streams_count);

        Ok(Self {
            id,
            accessory_information,
            microphone,
            streams,
        })
    }
}

/// Video Doorbell Accessory: a primary Doorbell service plus the camera
/// services. iOS only shows it as a video doorbell when it is published by
/// its own (non bridged) server with the `VideoDoorbell` category.
#[derive(Default, Debug)]
pub struct VideoDoorbellAccessory {
    /// ID of the Video Doorbell accessory.
    id: u64,

    /// Accessory Information service.
    pub accessory_information: AccessoryInformationService,

    /// Doorbell service, rings through its Programmable Switch Event.
    pub doorbell: DoorbellService,

    /// Microphone
    pub microphone: MicrophoneService,

    /// Speaker
    pub speaker: SpeakerService,

    /// Camera streams
    pub streams: Vec<CameraStreamManagementService>,
}

impl HapAccessory for VideoDoorbellAccessory {
    fn get_id(&self) -> u64 { self.id }

    fn set_id(&mut self, id: u64) { self.id = id; }

    fn get_service(&self, hap_type: HapType) -> Option<&dyn HapService> { find_service(self.get_services(), hap_type) }

    fn get_mut_service(&mut self, hap_type: HapType) -> Option<&mut dyn HapService> {
        find_mut_service(self.get_mut_services(), hap_type)
    }

    fn get_services(&self) -> Vec<&dyn HapService> {
        let mut services: Vec<&dyn HapService> = vec![
            &self.accessory_information,
            &self.doorbell,
            &self.microphone,
            &self.speaker,
        ];
        services.extend(self.streams.iter().map(|s| s as &dyn HapService));
        services
    }

    fn get_mut_services(&mut self) -> Vec<&mut dyn HapService> {
        let mut services: Vec<&mut dyn HapService> = vec![
            &mut self.accessory_information,
            &mut self.doorbell,
            &mut self.microphone,
            &mut self.speaker,
        ];
        services.extend(self.streams.iter_mut().map(|s| s as &mut dyn HapService));
        services
    }
}

impl CameraStreamAccessory for VideoDoorbellAccessory {
    fn camera_streams_mut(&mut self) -> Vec<&mut CameraStreamManagementService> { self.streams.iter_mut().collect() }
}

impl Serialize for VideoDoorbellAccessory {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HapAccessory", 2)?;
        state.serialize_field("aid", &self.get_id())?;
        state.serialize_field("services", &self.get_services())?;
        state.end()
    }
}

impl VideoDoorbellAccessory {
    /// Creates a new Video Doorbell Accessory.
    pub fn new(id: u64, streams_count: usize, information: AccessoryInformation) -> Result<Self> {
        let accessory_information = information.to_service(1, id)?;
        let mut iids = IidAllocator(1);
        iids.next_for(&accessory_information);

        let mut doorbell = DoorbellService::new(iids.0, id);
        doorbell.set_primary(true);
        iids.next_for(&doorbell);

        let microphone = MicrophoneService::new(iids.0, id);
        iids.next_for(&microphone);

        let speaker = new_speaker(&mut iids, id);

        let streams = new_streams(&mut iids, id, streams_count);

        Ok(Self {
            id,
            accessory_information,
            doorbell,
            microphone,
            speaker,
            streams,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn iids(accessory: &dyn HapAccessory) -> Vec<u64> {
        accessory
            .get_services()
            .into_iter()
            .flat_map(|s| std::iter::once(s.get_id()).chain(s.get_characteristics().into_iter().map(|c| c.get_id())))
            .collect()
    }

    #[test]
    fn video_doorbell_instance_ids_are_unique() {
        let doorbell = VideoDoorbellAccessory::new(1, 2, AccessoryInformation {
            name: "Door".into(),
            firmware_revision: Some("1.0.0".into()),
            software_revision: Some("1.0.0".into()),
            ..Default::default()
        })
        .unwrap();
        let ids = iids(&doorbell);
        assert_eq!(ids.len(), ids.iter().collect::<HashSet<_>>().len());
        assert!(doorbell.doorbell.get_primary());
        assert_eq!(doorbell.streams.len(), 2);
        assert!(doorbell.get_service(HapType::Speaker).is_some());
    }

    #[test]
    fn camera_instance_ids_are_unique() {
        let camera = CameraAccessory::new(1, 1, AccessoryInformation {
            name: "Cam".into(),
            firmware_revision: Some("1.0.0".into()),
            ..Default::default()
        })
        .unwrap();
        let ids = iids(&camera);
        assert_eq!(ids.len(), ids.iter().collect::<HashSet<_>>().len());
    }
}
