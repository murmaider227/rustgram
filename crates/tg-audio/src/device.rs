//! Audio device enumeration, resolution, and default-device following.
//!
//! Port of `lib_webrtc/webrtc/webrtc_device_common.h`,
//! `webrtc_device_resolver.cpp`, and `webrtc_environment.h` backed by `cpal`.

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait};

use crate::error::{AudioError, AudioResult};

/// Sentinel device ID representing the system default device (`kDefaultDeviceId`).
pub const DEFAULT_DEVICE_ID: &str = "default";

/// Direction of an audio device (`Webrtc::DeviceType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceType {
    /// Audio output / speakers / headphones.
    Playback,
    /// Audio input / microphone.
    Capture,
}

/// Reason a resolved audio device changed (`Webrtc::DeviceChangeReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DeviceChangeReason {
    /// User explicitly changed the preferred device or system default changed manually.
    #[default]
    Manual,
    /// A preferred or new default device was connected.
    Connected,
    /// The active device was disconnected and fell back to default.
    Disconnected,
}

/// Metadata describing an available audio device (`Webrtc::DeviceInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Stable device identifier string.
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// Playback or capture direction.
    pub device_type: DeviceType,
    /// Whether this device is currently the OS default for its direction.
    pub is_default: bool,
    /// Whether the device is currently inactive/unavailable.
    pub inactive: bool,
}

/// Result of resolving a saved device preference against current OS devices
/// (`Webrtc::DeviceResolvedId`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceResolvedId {
    /// Concrete device ID (or empty / [`DEFAULT_DEVICE_ID`] if none available).
    pub value: String,
    /// Playback or capture direction.
    pub device_type: DeviceType,
    /// `true` when resolved via system default (either requested or via fallback).
    pub computed_from_default: bool,
}

impl DeviceResolvedId {
    /// Returns `true` if this resolved ID tracks the system default device.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.computed_from_default || self.value.is_empty() || self.value == DEFAULT_DEVICE_ID
    }
}

/// Normalizes an empty saved device preference to [`DEFAULT_DEVICE_ID`].
///
/// Port of `Webrtc::DeviceIdOrDefault`.
#[must_use]
pub fn device_id_or_default(id: &str) -> String {
    if id.is_empty() {
        DEFAULT_DEVICE_ID.to_string()
    } else {
        id.to_string()
    }
}

/// Returns `true` if `id` refers to the system default device.
#[must_use]
pub fn is_default_device_id(id: &str) -> bool {
    id.is_empty() || id == DEFAULT_DEVICE_ID
}

/// Pure resolver implementing `Webrtc::DeviceResolver` selection & fallback rules.
///
/// Tracks a saved preferred device ID (`"default"` or a concrete device ID),
/// resolves it against the live device list and current system default, and
/// records the [`DeviceChangeReason`] on transitions.
#[derive(Debug, Clone)]
pub struct DeviceResolver {
    device_type: DeviceType,
    saved_id: String,
    current: DeviceResolvedId,
    last_change_reason: DeviceChangeReason,
}

impl DeviceResolver {
    /// Creates a new resolver for `device_type` with initial preference `saved_id`.
    #[must_use]
    pub fn new(device_type: DeviceType, saved_id: impl Into<String>) -> Self {
        let saved_id = device_id_or_default(&saved_id.into());
        Self {
            device_type,
            saved_id,
            current: DeviceResolvedId {
                value: String::new(),
                device_type,
                computed_from_default: true,
            },
            last_change_reason: DeviceChangeReason::Manual,
        }
    }

    /// Returns the saved preference (`"default"` or specific device ID).
    #[must_use]
    pub fn saved_id(&self) -> &str {
        &self.saved_id
    }

    /// Returns the last resolved device ID.
    #[must_use]
    pub fn current(&self) -> &DeviceResolvedId {
        &self.current
    }

    /// Returns the reason for the most recent resolved device change.
    #[must_use]
    pub const fn last_change_reason(&self) -> DeviceChangeReason {
        self.last_change_reason
    }

    /// Updates the user's preferred device ID (`""` or `"default"` means follow OS default)
    /// and re-resolves against `devices` and `default_id`.
    ///
    /// Returns `true` if the resolved device changed.
    pub fn set_saved_id(
        &mut self,
        saved_id: impl Into<String>,
        devices: &[DeviceInfo],
        default_id: &str,
    ) -> bool {
        self.saved_id = device_id_or_default(&saved_id.into());
        self.last_change_reason = DeviceChangeReason::Manual;
        self.resolve(devices, default_id, DeviceChangeReason::Manual)
    }

    /// Re-resolves the active device when the system device list or default changes.
    ///
    /// Exact port of `DeviceResolver::trackSavedId` (`webrtc_device_resolver.cpp:43-81`).
    /// Returns `true` if the resolved device changed.
    pub fn resolve(
        &mut self,
        devices: &[DeviceInfo],
        default_id: &str,
        default_change_reason: DeviceChangeReason,
    ) -> bool {
        let prev = self.current.clone();
        if is_default_device_id(&self.saved_id) {
            let next = DeviceResolvedId {
                value: default_id.to_string(),
                device_type: self.device_type,
                computed_from_default: true,
            };
            if next != prev {
                self.last_change_reason = default_change_reason;
                self.current = next;
                return true;
            }
            return false;
        }

        if let Some(found) = devices
            .iter()
            .find(|d| d.id == self.saved_id && !d.inactive)
        {
            let next = DeviceResolvedId {
                value: found.id.clone(),
                device_type: self.device_type,
                computed_from_default: false,
            };
            if next != prev {
                self.last_change_reason = DeviceChangeReason::Connected;
                self.current = next;
                return true;
            }
            return false;
        }

        // Preferred device is missing or inactive: fall back to system default.
        let was_preferred = !prev.computed_from_default && prev.value == self.saved_id;
        let next = DeviceResolvedId {
            value: default_id.to_string(),
            device_type: self.device_type,
            computed_from_default: true,
        };
        if next != prev {
            self.last_change_reason = if was_preferred {
                DeviceChangeReason::Disconnected
            } else {
                default_change_reason
            };
            self.current = next;
            return true;
        }
        false
    }
}

/// Snapshot of system audio devices and active playback/capture selections,
/// backed by `cpal::default_host()`.
pub struct DeviceManager {
    host: cpal::Host,
    playback: Mutex<DeviceResolver>,
    capture: Mutex<DeviceResolver>,
}

impl DeviceManager {
    /// Creates a new [`DeviceManager`] using the platform's default `cpal` host
    /// (WASAPI on Windows) and following OS default playback & capture devices.
    #[must_use]
    pub fn new() -> Self {
        let host = cpal::default_host();
        let manager = Self {
            host,
            playback: Mutex::new(DeviceResolver::new(DeviceType::Playback, DEFAULT_DEVICE_ID)),
            capture: Mutex::new(DeviceResolver::new(DeviceType::Capture, DEFAULT_DEVICE_ID)),
        };
        let _ = manager.refresh(DeviceType::Playback);
        let _ = manager.refresh(DeviceType::Capture);
        manager
    }

    /// Creates a shared `Arc<DeviceManager>`.
    #[must_use]
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Enumerates currently available devices of `device_type`.
    pub fn devices(&self, device_type: DeviceType) -> AudioResult<Vec<DeviceInfo>> {
        let default_id = self.default_device_id(device_type);
        let iter = match device_type {
            DeviceType::Playback => self
                .host
                .output_devices()
                .map_err(|e| AudioError::Device(e.to_string()))?
                .collect::<Vec<_>>(),
            DeviceType::Capture => self
                .host
                .input_devices()
                .map_err(|e| AudioError::Device(e.to_string()))?
                .collect::<Vec<_>>(),
        };

        let mut list = Vec::with_capacity(iter.len());
        for dev in iter {
            let id = device_stable_id(&dev);
            let name = device_display_name(&dev);
            let is_default = !default_id.is_empty() && id == default_id;
            list.push(DeviceInfo {
                id,
                name,
                device_type,
                is_default,
                inactive: false,
            });
        }
        Ok(list)
    }

    /// Returns the OS default device ID string for `device_type` (empty if none).
    #[must_use]
    pub fn default_device_id(&self, device_type: DeviceType) -> String {
        let dev = match device_type {
            DeviceType::Playback => self.host.default_output_device(),
            DeviceType::Capture => self.host.default_input_device(),
        };
        dev.map(|d| device_stable_id(&d)).unwrap_or_default()
    }

    /// Sets the user's preferred device ID (`"default"` or `""` to follow OS default).
    /// Returns `true` if the resolved device changed.
    pub fn set_preferred_device(
        &self,
        device_type: DeviceType,
        saved_id: impl Into<String>,
    ) -> AudioResult<bool> {
        let list = self.devices(device_type).unwrap_or_default();
        let default_id = self.default_device_id(device_type);
        let lock = match device_type {
            DeviceType::Playback => &self.playback,
            DeviceType::Capture => &self.capture,
        };
        let mut resolver = lock
            .lock()
            .map_err(|_| AudioError::Device("device resolver lock poisoned".into()))?;
        Ok(resolver.set_saved_id(saved_id, &list, &default_id))
    }

    /// Refreshes devices from the OS and re-resolves the active device.
    /// Returns `Some(new_resolved)` if the active device changed.
    pub fn refresh(&self, device_type: DeviceType) -> AudioResult<Option<DeviceResolvedId>> {
        let list = self.devices(device_type).unwrap_or_default();
        let default_id = self.default_device_id(device_type);
        let lock = match device_type {
            DeviceType::Playback => &self.playback,
            DeviceType::Capture => &self.capture,
        };
        let mut resolver = lock
            .lock()
            .map_err(|_| AudioError::Device("device resolver lock poisoned".into()))?;
        if resolver.resolve(&list, &default_id, DeviceChangeReason::Manual) {
            Ok(Some(resolver.current().clone()))
        } else {
            Ok(None)
        }
    }

    /// Returns the currently resolved playback or capture device ID.
    #[must_use]
    pub fn resolved(&self, device_type: DeviceType) -> DeviceResolvedId {
        let lock = match device_type {
            DeviceType::Playback => &self.playback,
            DeviceType::Capture => &self.capture,
        };
        lock.lock()
            .map(|r| r.current().clone())
            .unwrap_or(DeviceResolvedId {
                value: String::new(),
                device_type,
                computed_from_default: true,
            })
    }

    /// Resolves and opens the underlying `cpal::Device` matching the current
    /// resolved preference (falling back to OS default if needed).
    pub fn open_cpal_device(&self, device_type: DeviceType) -> AudioResult<cpal::Device> {
        let _ = self.refresh(device_type);
        let resolved = self.resolved(device_type);

        if !resolved.is_default() && !resolved.value.is_empty() {
            let devices = match device_type {
                DeviceType::Playback => self
                    .host
                    .output_devices()
                    .ok()
                    .map(|d| d.collect::<Vec<_>>()),
                DeviceType::Capture => self
                    .host
                    .input_devices()
                    .ok()
                    .map(|d| d.collect::<Vec<_>>()),
            };
            if let Some(devices) = devices
                && let Some(found) = devices.into_iter().find(|d| {
                    device_stable_id(d) == resolved.value
                        || device_display_name(d) == resolved.value
                })
            {
                return Ok(found);
            }
        }

        let default = match device_type {
            DeviceType::Playback => self.host.default_output_device(),
            DeviceType::Capture => self.host.default_input_device(),
        };
        default.ok_or(AudioError::NoDevice)
    }

    /// Returns `true` if at least one playback device is available.
    #[must_use]
    pub fn has_playback_device(&self) -> bool {
        self.host.default_output_device().is_some()
    }

    /// Returns `true` if at least one capture (microphone) device is available.
    #[must_use]
    pub fn has_capture_device(&self) -> bool {
        self.host.default_input_device().is_some()
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

fn device_stable_id(device: &cpal::Device) -> String {
    if let Ok(id) = device.id() {
        return id.to_string();
    }
    device_display_name(device)
}

fn device_display_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Unknown Audio Device".to_string())
}
