//! Windows Media Foundation camera capture, H.264 MFT encode/decode, and DXGI screen capture.
//!
//! Reference:
//! - Windows Media Foundation `MFEnumDeviceSources` / `IMFSourceReader` (camera capture)
//! - Windows DXGI Desktop Duplication `IDXGIOutput1::DuplicateOutput` (screen sharing)
//! - Windows Media Foundation `MFTEnumEx` / `IMFTransform` / `ICodecAPI` (hardware H.264 MFT
//!   with automatic fallback to Windows' built-in software Microsoft H.264 MFT)
//!
//! Zero CMake, zero OpenH264 binary downloads, pure Win32 COM via the `windows` crate.

#![cfg(windows)]

use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{E_NOINTERFACE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMF2DBuffer,
    IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaEventGenerator, IMFMediaSource,
    IMFMediaType, IMFSample, IMFSourceReader, IMFTransform, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
    METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_TRANSFORM_ASYNC,
    MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFCreateAttributes, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources,
    MFMediaType_Video, MFSTARTUP_NOSOCKET, MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_DECODER,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_I420, MFVideoFormat_NV12,
    MFVideoFormat_RGB32, MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR,
    eAVEncH264VProfile_Base,
};
use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Variant::VARIANT;
use windows_core::{GUID, Interface, PWSTR};

use crate::error::{VoipError, VoipResult};
use crate::video::{
    CameraCaptureConfig, CameraDeviceInfo, DisplayDeviceInfo, VideoCaptureSource, VideoCodecKind,
    VideoDecoder, VideoEncoder, VideoEncoderConfig, VideoFrame, VideoPixelFormat, nv12_buffer_len,
    resize_nv12,
};

struct ThreadComGuard {
    owned: bool,
}

impl Drop for ThreadComGuard {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: Paired with `CoInitializeEx` on this exact thread during thread exit.
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    static THREAD_COM: std::cell::OnceCell<Result<ThreadComGuard, String>> =
        const { std::cell::OnceCell::new() };
}

fn ensure_thread_com_init() -> VoipResult<()> {
    THREAD_COM.with(|cell| {
        let res = cell.get_or_init(|| {
            // SAFETY: Initializes COM once per thread; `RPC_E_CHANGED_MODE` means COM is already
            // initialized on this thread (e.g. STA UI thread), which is safe to share.
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_ok() {
                Ok(ThreadComGuard { owned: true })
            } else if hr == RPC_E_CHANGED_MODE {
                Ok(ThreadComGuard { owned: false })
            } else {
                Err(format!("CoInitializeEx failed: {hr:?}"))
            }
        });
        match res {
            Ok(_) => Ok(()),
            Err(msg) => Err(VoipError::Video(msg.clone())),
        }
    })
}

/// RAII guard for process-wide reference-counted Windows Media Foundation lifetime.
///
/// Must always be declared as the **last** field of any struct holding COM/MF interfaces so that
/// all MF objects are released before `MFShutdown()` runs.
struct MfGuard;

impl MfGuard {
    fn init() -> VoipResult<Self> {
        ensure_thread_com_init()?;
        // SAFETY: `MFStartup` is reference-counted and paired with `MFShutdown` in `Drop`.
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) }
            .map_err(|e| VoipError::Video(format!("MFStartup failed: {e}")))?;
        Ok(Self)
    }
}

impl Drop for MfGuard {
    fn drop(&mut self) {
        // SAFETY: Paired with `MFStartup` in `MfGuard::init`.
        let _ = unsafe { MFShutdown() };
    }
}

const fn pack_u64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | (lo as u64)
}

const fn unpack_u64(val: u64) -> (u32, u32) {
    ((val >> 32) as u32, (val & 0xFFFF_FFFF) as u32)
}

fn read_allocated_mf_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let mut pwstr = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: `GetAllocatedString` allocates a CoTaskMem null-terminated UTF-16 string freed below.
    let res = unsafe { attrs.GetAllocatedString(key, &mut pwstr, &mut len) };
    if res.is_err() || pwstr.is_null() {
        return None;
    }
    // SAFETY: `pwstr` is valid for `len` UTF-16 code units as guaranteed by `GetAllocatedString`.
    let out = unsafe { pwstr.to_string().ok() };
    // SAFETY: `pwstr` was allocated by `GetAllocatedString` using the COM task allocator.
    unsafe { CoTaskMemFree(Some(pwstr.0 as *const _)) };
    out
}

/// Take ownership of a COM `CoTaskMem`-allocated array of `Option<IMFActivate>` returned by
/// `MFEnumDeviceSources` or `MFTEnumEx`, releasing the outer task memory cleanly.
unsafe fn collect_activates(ptr: *mut Option<IMFActivate>, count: u32) -> Vec<IMFActivate> {
    if ptr.is_null() || count == 0 {
        if !ptr.is_null() {
            // SAFETY: Caller passed a non-null CoTaskMem pointer from MF enumeration.
            unsafe { CoTaskMemFree(Some(ptr as *const _)) };
        }
        return Vec::new();
    }
    let mut result = Vec::with_capacity(count as usize);
    // SAFETY: `ptr` points to `count` initialized `Option<IMFActivate>` slots from MF.
    let slice = unsafe { std::slice::from_raw_parts_mut(ptr, count as usize) };
    for slot in slice.iter_mut() {
        if let Some(act) = slot.take() {
            result.push(act);
        }
    }
    // SAFETY: All COM interface pointers were moved out (`take()`), so freeing the outer buffer is safe.
    unsafe { CoTaskMemFree(Some(ptr as *const _)) };
    result
}

// ---------------------------------------------------------------------------
// 1. Camera Enumeration & Capture (`MFEnumDeviceSources` + `IMFSourceReader`)
// ---------------------------------------------------------------------------

/// Enumerate available video capture cameras via Windows Media Foundation `MFEnumDeviceSources`.
pub fn mf_list_cameras() -> VoipResult<Vec<CameraDeviceInfo>> {
    let _guard = MfGuard::init()?;
    let activates = enum_camera_activates()?;
    let mut devices = Vec::with_capacity(activates.len());
    for (idx, act) in activates.iter().enumerate() {
        let attrs: IMFAttributes = act
            .cast()
            .map_err(|e| VoipError::Video(format!("IMFActivate -> IMFAttributes failed: {e}")))?;
        let name = read_allocated_mf_string(&attrs, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
            .unwrap_or_else(|| format!("Camera {}", idx + 1));
        let id = read_allocated_mf_string(
            &attrs,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
        )
        .unwrap_or_else(|| format!("camera-{idx}"));
        devices.push(CameraDeviceInfo { id, name });
    }
    Ok(devices)
}

fn enum_camera_activates() -> VoipResult<Vec<IMFActivate>> {
    let mut attrs: Option<IMFAttributes> = None;
    // SAFETY: Standard Media Foundation attribute store creation.
    unsafe { MFCreateAttributes(&mut attrs, 1) }
        .map_err(|e| VoipError::Video(format!("MFCreateAttributes failed: {e}")))?;
    let attrs = attrs.ok_or_else(|| VoipError::Video("null IMFAttributes".into()))?;
    // SAFETY: Setting standard video capture device filter attribute.
    unsafe {
        attrs.SetGUID(
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
            &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
        )
    }
    .map_err(|e| VoipError::Video(format!("SetGUID vidcap failed: {e}")))?;

    let mut raw_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: `MFEnumDeviceSources` writes `raw_ptr` and `count`, freed by `collect_activates`.
    unsafe { MFEnumDeviceSources(&attrs, &mut raw_ptr, &mut count) }
        .map_err(|e| VoipError::Video(format!("MFEnumDeviceSources failed: {e}")))?;
    // SAFETY: `raw_ptr` and `count` come directly from `MFEnumDeviceSources`.
    Ok(unsafe { collect_activates(raw_ptr, count) })
}

/// Windows Media Foundation camera capture source (`IMFSourceReader`).
pub struct MfCameraCapture {
    reader: Option<IMFSourceReader>,
    source: Option<IMFMediaSource>,
    width: u32,
    height: u32,
    format: VideoPixelFormat,
    _guard: MfGuard,
}

// SAFETY: `MfCameraCapture` is exclusively accessed via `&mut self` (owned by one capture thread/task)
// and Media Foundation `IMFSourceReader` / `IMFMediaSource` COM objects are free-threaded / MTA safe.
unsafe impl Send for MfCameraCapture {}

impl MfCameraCapture {
    /// Open a camera by `device_id` (`""` or `"default"` selects the first available camera).
    pub fn open(device_id: &str, config: CameraCaptureConfig) -> VoipResult<Self> {
        let guard = MfGuard::init()?;
        let activates = enum_camera_activates()?;
        if activates.is_empty() {
            return Err(VoipError::Video("no camera devices found".into()));
        }

        let selected = if device_id.is_empty() || device_id == "default" {
            activates[0].clone()
        } else {
            activates
                .iter()
                .find(|act| {
                    act.cast::<IMFAttributes>()
                        .ok()
                        .and_then(|a| {
                            read_allocated_mf_string(
                                &a,
                                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                            )
                        })
                        .is_some_and(|id| id == device_id)
                })
                .cloned()
                .ok_or_else(|| VoipError::Video(format!("camera '{device_id}' not found")))?
        };

        // SAFETY: `ActivateObject` instantiates the `IMFMediaSource` for the selected camera.
        let source: IMFMediaSource = unsafe { selected.ActivateObject() }
            .map_err(|e| VoipError::Video(format!("ActivateObject(IMFMediaSource) failed: {e}")))?;

        // SAFETY: Creates a synchronous `IMFSourceReader` bound to `source`.
        let reader = unsafe { MFCreateSourceReaderFromMediaSource(&source, None) }
            .map_err(|e| VoipError::Video(format!("MFCreateSourceReader failed: {e}")))?;

        let stream_idx = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let (format, width, height) = configure_camera_output(&reader, stream_idx, config)?;

        Ok(Self {
            _guard: guard,
            reader: Some(reader),
            source: Some(source),
            width,
            height,
            format,
        })
    }
}

fn configure_camera_output(
    reader: &IMFSourceReader,
    stream_idx: u32,
    config: CameraCaptureConfig,
) -> VoipResult<(VideoPixelFormat, u32, u32)> {
    let candidates = [
        (MFVideoFormat_NV12, VideoPixelFormat::Nv12),
        (MFVideoFormat_RGB32, VideoPixelFormat::Bgra),
    ];

    for (subtype, fmt) in candidates {
        // SAFETY: Creates an empty `IMFMediaType` configured below.
        let Ok(mt) = (unsafe { MFCreateMediaType() }) else {
            continue;
        };
        // SAFETY: Setting standard MF media type GUIDs and dimensions.
        let configured = unsafe {
            let _ = mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
            let _ = mt.SetGUID(&MF_MT_SUBTYPE, &subtype);
            let _ = mt.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height));
            let _ = mt.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps.max(1), 1));
            reader.SetCurrentMediaType(stream_idx, None, &mt)
        };
        let ok = if configured.is_ok() {
            true
        } else {
            // Retry without forcing exact dimensions/fps so the camera picks its closest mode.
            // SAFETY: Rebuilding minimal subtype-only media type.
            unsafe {
                if let Ok(mt2) = MFCreateMediaType() {
                    let _ = mt2.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
                    let _ = mt2.SetGUID(&MF_MT_SUBTYPE, &subtype);
                    reader.SetCurrentMediaType(stream_idx, None, &mt2).is_ok()
                } else {
                    false
                }
            }
        };

        if ok {
            // SAFETY: Query actual negotiated frame dimensions.
            let current = unsafe { reader.GetCurrentMediaType(stream_idx) }
                .map_err(|e| VoipError::Video(format!("GetCurrentMediaType failed: {e}")))?;
            let (w, h) = unsafe { current.GetUINT64(&MF_MT_FRAME_SIZE) }
                .map(unpack_u64)
                .unwrap_or((config.width, config.height));
            return Ok((fmt, w, h));
        }
    }

    Err(VoipError::Video(
        "failed to configure camera source reader for NV12 or RGB32".into(),
    ))
}

impl VideoCaptureSource for MfCameraCapture {
    fn next_frame(&mut self) -> VoipResult<Option<VideoFrame>> {
        let Some(reader) = &self.reader else {
            return Ok(None);
        };
        let mut flags = 0u32;
        let mut timestamp_100ns = 0i64;
        let mut sample: Option<IMFSample> = None;

        // SAFETY: Standard synchronous `IMFSourceReader::ReadSample` call.
        unsafe {
            reader.ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                None,
                Some(&mut flags),
                Some(&mut timestamp_100ns),
                Some(&mut sample),
            )
        }
        .map_err(|e| VoipError::Video(format!("IMFSourceReader::ReadSample failed: {e}")))?;

        let Some(sample) = sample else {
            return Ok(None);
        };
        let data = extract_sample_bytes(&sample, self.width, self.height, self.format)?;
        Ok(Some(VideoFrame {
            width: self.width,
            height: self.height,
            rotation: 0,
            timestamp_us: (timestamp_100ns.max(0) as u64) / 10,
            format: self.format,
            data,
        }))
    }

    fn stop(&mut self) {
        self.reader = None;
        if let Some(src) = self.source.take() {
            // SAFETY: Cleanly shut down the camera media source.
            let _ = unsafe { src.Shutdown() };
        }
    }
}

impl Drop for MfCameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// 2. Screen Capture via DXGI Desktop Duplication (`IDXGIOutput1::DuplicateOutput`)
// ---------------------------------------------------------------------------

/// Enumerate attached desktop outputs via DXGI (`CreateDXGIFactory1`).
pub fn dxgi_list_displays() -> VoipResult<Vec<DisplayDeviceInfo>> {
    // SAFETY: Standard DXGI factory creation.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
        .map_err(|e| VoipError::Video(format!("CreateDXGIFactory1 failed: {e}")))?;

    let mut displays = Vec::new();
    let mut adapter_idx = 0u32;
    loop {
        // SAFETY: Iterating DXGI adapters until `DXGI_ERROR_NOT_FOUND`.
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(adapter_idx) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(VoipError::Video(format!("EnumAdapters1 failed: {e}"))),
        };
        adapter_idx += 1;

        let mut output_idx = 0u32;
        loop {
            // SAFETY: Iterating outputs on this adapter until `DXGI_ERROR_NOT_FOUND`.
            let output: IDXGIOutput = match unsafe { adapter.EnumOutputs(output_idx) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(_) => break,
            };
            output_idx += 1;

            // SAFETY: Retrieving output descriptor.
            if let Ok(desc) = unsafe { output.GetDesc() } {
                let raw_name = &desc.DeviceName;
                let nul = raw_name
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(raw_name.len());
                let id = String::from_utf16_lossy(&raw_name[..nul]);
                let rect = desc.DesktopCoordinates;
                let width = (rect.right - rect.left).max(0) as u32;
                let height = (rect.bottom - rect.top).max(0) as u32;
                let name = format!("Display {} ({width}x{height})", displays.len() + 1);
                displays.push(DisplayDeviceInfo {
                    id,
                    name,
                    width,
                    height,
                    left: rect.left,
                    top: rect.top,
                });
            }
        }
    }
    Ok(displays)
}

/// Desktop screen-sharing capture source backed by Windows DXGI Desktop Duplication.
pub struct DxgiScreenCapture {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: Option<IDXGIOutputDuplication>,
    staging: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
    start_instant: std::time::Instant,
}

// SAFETY: `DxgiScreenCapture` is exclusively driven via `&mut self` (single owner); D3D11 device
// and immediate context are safe to move between threads when synchronized by exclusive `&mut self`.
unsafe impl Send for DxgiScreenCapture {}

impl DxgiScreenCapture {
    /// Open DXGI Desktop Duplication for `display_id` (`""` or `"default"` uses the first output).
    pub fn open(display_id: &str) -> VoipResult<Self> {
        // SAFETY: Creating DXGI 1.1 factory.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }
            .map_err(|e| VoipError::Video(format!("CreateDXGIFactory1 failed: {e}")))?;

        let mut chosen: Option<(IDXGIAdapter1, IDXGIOutput, u32, u32)> = None;
        let mut adapter_idx = 0u32;
        'outer: loop {
            // SAFETY: Enumerating DXGI adapters.
            let adapter = match unsafe { factory.EnumAdapters1(adapter_idx) } {
                Ok(a) => a,
                Err(_) => break,
            };
            adapter_idx += 1;
            let mut output_idx = 0u32;
            loop {
                // SAFETY: Enumerating DXGI outputs on adapter.
                let output = match unsafe { adapter.EnumOutputs(output_idx) } {
                    Ok(o) => o,
                    Err(_) => break,
                };
                output_idx += 1;
                if let Ok(desc) = unsafe { output.GetDesc() } {
                    let nul = desc
                        .DeviceName
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(desc.DeviceName.len());
                    let id = String::from_utf16_lossy(&desc.DeviceName[..nul]);
                    let w = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left).max(0)
                        as u32;
                    let h = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top).max(0)
                        as u32;
                    if display_id.is_empty() || display_id == "default" || id == display_id {
                        chosen = Some((adapter, output, w, h));
                        break 'outer;
                    }
                }
            }
        }

        let (adapter1, output, width, height) =
            chosen.ok_or_else(|| VoipError::Video(format!("display '{display_id}' not found")))?;

        let adapter: IDXGIAdapter = adapter1
            .cast()
            .map_err(|e| VoipError::Video(format!("IDXGIAdapter cast failed: {e}")))?;

        let feature_levels = [D3D_FEATURE_LEVEL_11_0];
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: Standard D3D11 device creation bound to the display's DXGI adapter.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| VoipError::Video(format!("D3D11CreateDevice failed: {e}")))?;

        let device = device.ok_or_else(|| VoipError::Video("null ID3D11Device".into()))?;
        let context = context.ok_or_else(|| VoipError::Video("null ID3D11DeviceContext".into()))?;

        let output1: IDXGIOutput1 = output
            .cast()
            .map_err(|e| VoipError::Video(format!("IDXGIOutput1 cast failed: {e}")))?;
        // SAFETY: Acquires desktop duplication interface for the selected display.
        let duplication = unsafe { output1.DuplicateOutput(&device) }
            .map_err(|e| VoipError::Video(format!("IDXGIOutput1::DuplicateOutput failed: {e}")))?;

        Ok(Self {
            device,
            context,
            duplication: Some(duplication),
            staging: None,
            width,
            height,
            start_instant: std::time::Instant::now(),
        })
    }

    fn ensure_staging(&mut self, w: u32, h: u32) -> VoipResult<ID3D11Texture2D> {
        if let Some(ref tex) = self.staging
            && self.width == w
            && self.height == h
        {
            return Ok(tex.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        // SAFETY: Creates CPU-readable staging texture matching the desktop surface.
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
            .map_err(|e| VoipError::Video(format!("CreateTexture2D(staging) failed: {e}")))?;
        let tex = tex.ok_or_else(|| VoipError::Video("null staging texture".into()))?;
        self.width = w;
        self.height = h;
        self.staging = Some(tex.clone());
        Ok(tex)
    }
}

impl VideoCaptureSource for DxgiScreenCapture {
    fn next_frame(&mut self) -> VoipResult<Option<VideoFrame>> {
        let Some(dupl) = self.duplication.clone() else {
            return Ok(None);
        };
        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;

        // SAFETY: Acquires next updated desktop frame with 16 ms timeout.
        let acquire_res = unsafe { dupl.AcquireNextFrame(16, &mut frame_info, &mut resource) };
        if let Err(e) = acquire_res {
            if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(None);
            }
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                self.duplication = None;
                return Err(VoipError::Video(
                    "DXGI desktop duplication access lost (mode switch / UAC)".into(),
                ));
            }
            return Err(VoipError::Video(format!("AcquireNextFrame failed: {e}")));
        }

        let copy_result = (|| -> VoipResult<VideoFrame> {
            let resource =
                resource.ok_or_else(|| VoipError::Video("null DXGI desktop resource".into()))?;
            let desktop_tex: ID3D11Texture2D = resource
                .cast()
                .map_err(|e| VoipError::Video(format!("IDXGIResource -> Texture2D failed: {e}")))?;

            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: Querying desktop texture dimensions.
            unsafe { desktop_tex.GetDesc(&mut desc) };
            let staging = self.ensure_staging(desc.Width, desc.Height)?;

            // SAFETY: GPU copy from duplicated desktop texture into CPU staging texture.
            unsafe { self.context.CopyResource(&staging, &desktop_tex) };

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // SAFETY: Mapping staging texture for synchronous CPU read, paired with `Unmap`.
            unsafe {
                self.context
                    .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            }
            .map_err(|e| VoipError::Video(format!("D3D11 Map failed: {e}")))?;

            let w = desc.Width as usize;
            let h = desc.Height as usize;
            let row_bytes = w * 4;
            let pitch = mapped.RowPitch as usize;
            let mut bgra = vec![0u8; row_bytes * h];
            if !mapped.pData.is_null() && pitch >= row_bytes {
                // SAFETY: `mapped.pData` is valid for `h * pitch` bytes while mapped.
                let src =
                    unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, h * pitch) };
                for y in 0..h {
                    let s_start = y * pitch;
                    let d_start = y * row_bytes;
                    bgra[d_start..d_start + row_bytes]
                        .copy_from_slice(&src[s_start..s_start + row_bytes]);
                }
            }
            // SAFETY: Paired with `Map` above.
            unsafe { self.context.Unmap(&staging, 0) };

            Ok(VideoFrame {
                width: desc.Width,
                height: desc.Height,
                rotation: 0,
                timestamp_us: self.start_instant.elapsed().as_micros() as u64,
                format: VideoPixelFormat::Bgra,
                data: bgra,
            })
        })();

        // SAFETY: Always release the acquired DXGI frame regardless of readback result.
        let _ = unsafe { dupl.ReleaseFrame() };
        copy_result.map(Some)
    }

    fn stop(&mut self) {
        self.duplication = None;
        self.staging = None;
    }
}

// ---------------------------------------------------------------------------
// 3. Windows Media Foundation H.264 Encoder MFT (HW first, SW fallback)
// ---------------------------------------------------------------------------

/// Enumerate H.264 MFT candidates: hardware MFTs first, followed by synchronous software MFTs.
fn enum_h264_mfts(is_encoder: bool) -> VoipResult<Vec<(IMFActivate, bool)>> {
    let category = if is_encoder {
        MFT_CATEGORY_VIDEO_ENCODER
    } else {
        MFT_CATEGORY_VIDEO_DECODER
    };
    let nv12 = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };
    let h264 = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let (in_type, out_type) = if is_encoder {
        (Some(&nv12 as *const _), Some(&h264 as *const _))
    } else {
        (Some(&h264 as *const _), Some(&nv12 as *const _))
    };

    let mut out = Vec::new();
    let flag_sets = [
        (
            MFT_ENUM_FLAG(
                MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
            ),
            true,
        ),
        (
            MFT_ENUM_FLAG(
                MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_LOCALMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0,
            ),
            false,
        ),
    ];

    for (flags, is_hw) in flag_sets {
        let mut raw_ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: `MFTEnumEx` writes `raw_ptr` and `count`, freed by `collect_activates`.
        if unsafe { MFTEnumEx(category, flags, in_type, out_type, &mut raw_ptr, &mut count) }
            .is_ok()
        {
            // SAFETY: `raw_ptr` and `count` come directly from `MFTEnumEx`.
            let list = unsafe { collect_activates(raw_ptr, count) };
            for act in list {
                out.push((act, is_hw));
            }
        }
    }
    Ok(out)
}

fn check_and_unlock_async_mft(mft: &IMFTransform) -> bool {
    // SAFETY: Querying MFT attributes for `MF_TRANSFORM_ASYNC`.
    let Ok(attrs) = (unsafe { mft.GetAttributes() }) else {
        return false;
    };
    let is_async = unsafe { attrs.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 0;
    if is_async {
        // SAFETY: Unlocking async MFT for direct `IMFMediaEventGenerator` driving.
        let _ = unsafe { attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) };
    }
    is_async
}

/// Windows Media Foundation H.264 video encoder (`IMFTransform`, hardware preferred with software fallback).
pub struct MfH264Encoder {
    mft: IMFTransform,
    event_gen: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    is_hardware: bool,
    mft_provides_samples: bool,
    output_buf_size: u32,
    pending_need_input: u32,
    config: VideoEncoderConfig,
    frame_index: u64,
    // Must be last so all MF COM objects release before `MFShutdown()`.
    _guard: MfGuard,
}

// SAFETY: `MfH264Encoder` is owned exclusively by one caller (`&mut self`) and Media Foundation
// MFTs are initialized under MTA COM.
unsafe impl Send for MfH264Encoder {}

struct ActivatedEncoderMft {
    mft: IMFTransform,
    event_gen: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    mft_provides_samples: bool,
    output_buf_size: u32,
}

impl MfH264Encoder {
    /// Instantiate and configure a Windows Media Foundation H.264 encoder (HW first, SW fallback).
    pub fn new(mut config: VideoEncoderConfig) -> VoipResult<Self> {
        let guard = MfGuard::init()?;
        config.width = (config.width & !1).max(16);
        config.height = (config.height & !1).max(16);
        config.fps = config.fps.max(1);
        config.bitrate_bps = config.bitrate_bps.max(64_000);

        let candidates = enum_h264_mfts(true)?;
        if candidates.is_empty() {
            return Err(VoipError::Video(
                "no Media Foundation H.264 encoder MFT available on this system".into(),
            ));
        }

        let mut last_err = String::from("no H.264 encoder candidate succeeded");
        for (act, is_hw) in candidates {
            match Self::try_activate_and_configure(&act, is_hw, config) {
                Ok(active) => {
                    tracing::info!(
                        "tg-voip initialized MF H.264 encoder (hardware={is_hw}, async={}, {}x{}@{}fps, {} bps)",
                        active.event_gen.is_some(),
                        config.width,
                        config.height,
                        config.fps,
                        config.bitrate_bps
                    );
                    return Ok(Self {
                        _guard: guard,
                        mft: active.mft,
                        event_gen: active.event_gen,
                        codec_api: active.codec_api,
                        is_hardware: is_hw,
                        mft_provides_samples: active.mft_provides_samples,
                        output_buf_size: active.output_buf_size,
                        pending_need_input: 0,
                        config,
                        frame_index: 0,
                    });
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
        }

        Err(VoipError::Video(format!(
            "all MF H.264 encoder MFTs failed: {last_err}"
        )))
    }

    /// Returns `true` if a hardware-accelerated MFT was selected (vs. software fallback).
    #[must_use]
    pub const fn is_hardware(&self) -> bool {
        self.is_hardware
    }

    fn try_activate_and_configure(
        act: &IMFActivate,
        _is_hw: bool,
        config: VideoEncoderConfig,
    ) -> VoipResult<ActivatedEncoderMft> {
        // SAFETY: Activates the candidate H.264 encoder MFT.
        let mft: IMFTransform = unsafe { act.ActivateObject() }
            .map_err(|e| VoipError::Video(format!("ActivateObject(H264 encoder) failed: {e}")))?;

        let is_async = check_and_unlock_async_mft(&mft);
        let event_gen = if is_async {
            Some(
                mft.cast::<IMFMediaEventGenerator>()
                    .map_err(|e| VoipError::Video(format!("async MFT missing event gen: {e}")))?,
            )
        } else {
            None
        };

        let codec_api = mft.cast::<ICodecAPI>().ok();

        // Configure H.264 output type FIRST (required by MF H.264 encoder MFTs).
        // SAFETY: Creating and populating H.264 output media type.
        let out_type: IMFMediaType = unsafe { MFCreateMediaType() }
            .map_err(|e| VoipError::Video(format!("MFCreateMediaType(out) failed: {e}")))?;
        (|| -> windows_core::Result<()> {
            // SAFETY: Setting standard MF video output attributes and configuring MFT output type.
            unsafe {
                out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                out_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
                out_type.SetUINT32(&MF_MT_AVG_BITRATE, config.bitrate_bps)?;
                out_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps, 1))?;
                out_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height))?;
                out_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                out_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
                mft.SetOutputType(0, &out_type, 0)?;
            }
            Ok(())
        })()
        .map_err(|e| VoipError::Video(format!("encoder SetOutputType(H264) failed: {e}")))?;

        // Configure NV12 input type SECOND.
        // SAFETY: Creating and populating NV12 input media type.
        let in_type: IMFMediaType = unsafe { MFCreateMediaType() }
            .map_err(|e| VoipError::Video(format!("MFCreateMediaType(in) failed: {e}")))?;
        (|| -> windows_core::Result<()> {
            // SAFETY: Setting standard MF NV12 input attributes and configuring MFT input type.
            unsafe {
                in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
                in_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(config.width, config.height))?;
                in_type.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(config.fps, 1))?;
                in_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                in_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
                mft.SetInputType(0, &in_type, 0)?;
            }
            Ok(())
        })()
        .map_err(|e| VoipError::Video(format!("encoder SetInputType(NV12) failed: {e}")))?;

        if let Some(ref api) = codec_api {
            // Best-effort low-latency real-time VoIP tuning via ICodecAPI.
            let _ = set_codec_api_bool(api, &CODECAPI_AVLowLatencyMode, true);
            let _ = set_codec_api_u32(
                api,
                &CODECAPI_AVEncCommonRateControlMode,
                eAVEncCommonRateControlMode_CBR.0 as u32,
            );
            let _ = set_codec_api_u32(api, &CODECAPI_AVEncCommonMeanBitRate, config.bitrate_bps);
            if config.gop_size > 0 {
                let _ = set_codec_api_u32(api, &CODECAPI_AVEncMPVGOPSize, config.gop_size);
            }
        }

        // SAFETY: Standard MFT streaming lifecycle notification.
        unsafe {
            let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }

        // SAFETY: Query whether the MFT allocates its own output samples.
        let out_info = unsafe { mft.GetOutputStreamInfo(0) }
            .map_err(|e| VoipError::Video(format!("GetOutputStreamInfo failed: {e}")))?;
        let provides = (out_info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32))
            != 0;
        let out_buf_size = out_info
            .cbSize
            .max(config.width * config.height)
            .max(65_536);

        Ok(ActivatedEncoderMft {
            mft,
            event_gen,
            codec_api,
            mft_provides_samples: provides,
            output_buf_size: out_buf_size,
        })
    }

    /// Invoke `IMFTransform::ProcessOutput` exactly once.
    ///
    /// Async MFTs require calling `ProcessOutput` strictly once per `METransformHaveOutput` event.
    fn pull_one_output(&self, out_bytes: &mut Vec<u8>) -> VoipResult<bool> {
        let sample = if self.mft_provides_samples {
            None
        } else {
            Some(create_output_sample(self.output_buf_size)?)
        };
        let mut buf = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: ManuallyDrop::new(sample),
            dwStatus: 0,
            pEvents: ManuallyDrop::new(None),
        };
        let mut status = 0u32;
        // SAFETY: Calling `ProcessOutput` on configured MFT.
        let res = unsafe {
            self.mft
                .ProcessOutput(0, std::slice::from_mut(&mut buf), &mut status)
        };
        // SAFETY: Take ownership back from `ManuallyDrop` so COM references are released cleanly.
        let out_sample = unsafe { ManuallyDrop::take(&mut buf.pSample) };
        let _out_events = unsafe { ManuallyDrop::take(&mut buf.pEvents) };

        match res {
            Ok(()) => {
                if let Some(s) = out_sample {
                    append_raw_sample_bytes(&s, out_bytes)?;
                }
                Ok(true)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
            Err(e) => Err(VoipError::Video(format!(
                "H.264 encoder ProcessOutput failed: {e}"
            ))),
        }
    }

    fn drain_sync_output(&self, out_bytes: &mut Vec<u8>) -> VoipResult<()> {
        while self.pull_one_output(out_bytes)? {}
        Ok(())
    }
    fn poll_async_event(
        event_gen: &IMFMediaEventGenerator,
        timeout: Duration,
    ) -> VoipResult<Option<u32>> {
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: Non-blocking `MF_EVENT_FLAG_NO_WAIT` polls the MFT event queue without
            // ever blocking indefinitely if a hardware MFT stalls or lacks a DXVA manager.
            match unsafe {
                event_gen.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(
                    MF_EVENT_FLAG_NO_WAIT.0,
                ))
            } {
                Ok(ev) => {
                    // SAFETY: Reading MF event type.
                    let ev_type = unsafe { ev.GetType() }.unwrap_or(0);
                    return Ok(Some(ev_type));
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => {
                    return Err(VoipError::Video(format!("async MFT GetEvent failed: {e}")));
                }
            }
        }
    }
}

impl VideoEncoder for MfH264Encoder {
    fn codec(&self) -> VideoCodecKind {
        VideoCodecKind::H264
    }

    fn encode(&mut self, frame: &VideoFrame, force_keyframe: bool) -> VoipResult<Vec<u8>> {
        let mut nv12 = frame.to_nv12_bytes()?;
        if frame.width != self.config.width || frame.height != self.config.height {
            nv12 = resize_nv12(
                &nv12,
                frame.width,
                frame.height,
                self.config.width,
                self.config.height,
            )?;
        }

        if force_keyframe && let Some(ref api) = self.codec_api {
            let _ = set_codec_api_u32(api, &CODECAPI_AVEncVideoForceKeyFrame, 1);
        }

        let duration_100ns = 10_000_000i64 / i64::from(self.config.fps.max(1));
        let timestamp_100ns = (self.frame_index as i64) * duration_100ns;
        self.frame_index += 1;

        let sample = create_input_sample(&nv12, timestamp_100ns, duration_100ns)?;
        let mut out_bytes = Vec::new();

        if let Some(ref event_gen) = self.event_gen {
            // Async hardware MFT: strictly 1 `ProcessInput` per `METransformNeedInput` and
            // strictly 1 `ProcessOutput` per `METransformHaveOutput`, polled with a bounded
            // non-blocking deadline so a stalled hardware MFT can never hang callers.
            let mut fed = false;
            if self.pending_need_input > 0 {
                self.pending_need_input -= 1;
                // SAFETY: Consuming previously queued `METransformNeedInput` credit.
                unsafe { self.mft.ProcessInput(0, &sample, 0) }
                    .map_err(|e| VoipError::Video(format!("async ProcessInput failed: {e}")))?;
                fed = true;
            }

            for _ in 0..8 {
                let Some(ev_type) = Self::poll_async_event(event_gen, Duration::from_millis(250))?
                else {
                    break;
                };
                if ev_type == METransformNeedInput.0 as u32 {
                    if !fed {
                        // SAFETY: Submitting input sample in response to `METransformNeedInput`.
                        unsafe { self.mft.ProcessInput(0, &sample, 0) }.map_err(|e| {
                            VoipError::Video(format!("async ProcessInput failed: {e}"))
                        })?;
                        fed = true;
                    } else {
                        // Encoder wants another input frame before emitting output; save credit.
                        self.pending_need_input += 1;
                        break;
                    }
                } else if ev_type == METransformHaveOutput.0 as u32 {
                    let _ = self.pull_one_output(&mut out_bytes)?;
                    if fed {
                        break;
                    }
                }
            }
        } else {
            // Synchronous MFT (Microsoft software H.264 encoder).
            // SAFETY: Submitting NV12 input sample.
            unsafe { self.mft.ProcessInput(0, &sample, 0) }
                .map_err(|e| VoipError::Video(format!("H.264 encoder ProcessInput failed: {e}")))?;
            self.drain_sync_output(&mut out_bytes)?;
        }

        Ok(out_bytes)
    }

    fn set_bitrate_bps(&mut self, bitrate_bps: u32) {
        let clamped = bitrate_bps.max(64_000);
        self.config.bitrate_bps = clamped;
        if let Some(ref api) = self.codec_api {
            let _ = set_codec_api_u32(api, &CODECAPI_AVEncCommonMeanBitRate, clamped);
        }
    }

    fn reconfigure(
        &mut self,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u32,
    ) -> VoipResult<()> {
        let w = (width & !1).max(16);
        let h = (height & !1).max(16);
        let f = fps.max(1);
        if w == self.config.width && h == self.config.height && f == self.config.fps {
            self.set_bitrate_bps(bitrate_bps);
            return Ok(());
        }
        let next_cfg = VideoEncoderConfig {
            width: w,
            height: h,
            fps: f,
            bitrate_bps: bitrate_bps.max(64_000),
            gop_size: f * 2,
        };
        let fresh = Self::new(next_cfg)?;
        *self = fresh;
        Ok(())
    }

    fn flush(&mut self) -> VoipResult<Vec<Vec<u8>>> {
        // SAFETY: Telling MFT to drain any buffered frames.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        let mut drained = Vec::new();
        if let Some(ref event_gen) = self.event_gen {
            for _ in 0..32 {
                let Ok(Some(ev_type)) =
                    Self::poll_async_event(event_gen, Duration::from_millis(250))
                else {
                    break;
                };
                if ev_type == METransformHaveOutput.0 as u32 {
                    let mut bytes = Vec::new();
                    if self.pull_one_output(&mut bytes).is_ok() && !bytes.is_empty() {
                        drained.push(bytes);
                    }
                } else if ev_type == METransformDrainComplete.0 as u32 {
                    break;
                }
            }
            self.pending_need_input = 0;
        } else {
            let mut bytes = Vec::new();
            let _ = self.drain_sync_output(&mut bytes);
            if !bytes.is_empty() {
                drained.push(bytes);
            }
        }
        // SAFETY: Flushing MFT state cleanly.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        Ok(drained)
    }
}

// ---------------------------------------------------------------------------
// 4. Windows Media Foundation H.264 Decoder MFT (HW first, SW fallback)
// ---------------------------------------------------------------------------

struct ActivatedDecoderMft {
    mft: IMFTransform,
    is_hardware: bool,
    mft_provides_samples: bool,
    width: u32,
    height: u32,
    stride: u32,
    output_format: VideoPixelFormat,
}

/// Windows Media Foundation H.264 video decoder (`IMFTransform`, hardware preferred with software fallback).
pub struct MfH264Decoder {
    mft: IMFTransform,
    is_hardware: bool,
    mft_provides_samples: bool,
    width: u32,
    height: u32,
    stride: u32,
    output_format: VideoPixelFormat,
    frame_counter: u64,
    // Must be last so all MF COM objects release before `MFShutdown()`.
    _guard: MfGuard,
}

// SAFETY: `MfH264Decoder` is owned exclusively by one caller (`&mut self`) under MTA COM.
unsafe impl Send for MfH264Decoder {}

impl MfH264Decoder {
    /// Instantiate a Windows Media Foundation H.264 decoder (prefers sync/SW or HW MFT).
    pub fn new() -> VoipResult<Self> {
        let guard = MfGuard::init()?;
        let candidates = enum_h264_mfts(false)?;
        if candidates.is_empty() {
            return Err(VoipError::Video(
                "no Media Foundation H.264 decoder MFT available on this system".into(),
            ));
        }

        // Prefer synchronous MFTs for direct low-latency decode without D3D11 DXVA device manager
        // requirements; still tries hardware if a sync hardware MFT is available.
        let mut ordered = candidates;
        ordered.sort_by_key(|(_, is_hw)| usize::from(*is_hw));

        let mut last_err = String::from("no H.264 decoder candidate succeeded");
        for (act, is_hw) in ordered {
            match Self::try_activate(&act, is_hw) {
                Ok(dec) => {
                    tracing::info!("tg-voip initialized MF H.264 decoder (hardware={is_hw})");
                    return Ok(Self {
                        mft: dec.mft,
                        is_hardware: dec.is_hardware,
                        mft_provides_samples: dec.mft_provides_samples,
                        width: dec.width,
                        height: dec.height,
                        stride: dec.stride,
                        output_format: dec.output_format,
                        frame_counter: 0,
                        _guard: guard,
                    });
                }
                Err(e) => last_err = e.to_string(),
            }
        }

        Err(VoipError::Video(format!(
            "all MF H.264 decoder MFTs failed: {last_err}"
        )))
    }

    fn try_activate(act: &IMFActivate, is_hw: bool) -> VoipResult<ActivatedDecoderMft> {
        // SAFETY: Activates candidate H.264 decoder MFT.
        let mft: IMFTransform = unsafe { act.ActivateObject() }
            .map_err(|e| VoipError::Video(format!("ActivateObject(H264 decoder) failed: {e}")))?;

        // Skip async-only decoders that require an external DXVA manager.
        if check_and_unlock_async_mft(&mft) {
            return Err(VoipError::Video(
                "skipping async-only H.264 decoder without DXVA manager".into(),
            ));
        }

        // Set H.264 input type FIRST.
        // SAFETY: Creating H.264 input media type.
        let in_type: IMFMediaType = unsafe { MFCreateMediaType() }
            .map_err(|e| VoipError::Video(format!("MFCreateMediaType(dec in) failed: {e}")))?;
        (|| -> windows_core::Result<()> {
            // SAFETY: Setting H.264 decoder input media type.
            unsafe {
                in_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                in_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
                mft.SetInputType(0, &in_type, 0)?;
            }
            Ok(())
        })()
        .map_err(|e| VoipError::Video(format!("decoder SetInputType(H264) failed: {e}")))?;

        let (width, height, stride, output_format) = select_decoder_output_type(&mft)?;

        // SAFETY: Notify MFT streaming start.
        unsafe {
            let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }

        // SAFETY: Query whether decoder allocates output samples.
        let out_info = unsafe { mft.GetOutputStreamInfo(0) }
            .map_err(|e| VoipError::Video(format!("decoder GetOutputStreamInfo failed: {e}")))?;
        let provides = (out_info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32))
            != 0;

        Ok(ActivatedDecoderMft {
            mft,
            is_hardware: is_hw,
            mft_provides_samples: provides,
            width,
            height,
            stride,
            output_format,
        })
    }

    /// Returns `true` if a hardware MFT was selected.
    #[must_use]
    pub const fn is_hardware(&self) -> bool {
        self.is_hardware
    }

    fn pull_one_output(&mut self) -> VoipResult<Option<VideoFrame>> {
        for _ in 0..3 {
            let sample = if self.mft_provides_samples {
                None
            } else {
                let min_bytes = nv12_buffer_len(self.width.max(64), self.height.max(64))
                    .max(self.stride as usize * self.height as usize * 2)
                    .max(65_536) as u32;
                Some(create_output_sample(min_bytes)?)
            };
            let mut buf = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            // SAFETY: Standard MFT `ProcessOutput`.
            let res = unsafe {
                self.mft
                    .ProcessOutput(0, std::slice::from_mut(&mut buf), &mut status)
            };
            // SAFETY: Reclaim ownership from `ManuallyDrop`.
            let out_sample = unsafe { ManuallyDrop::take(&mut buf.pSample) };
            let _out_events = unsafe { ManuallyDrop::take(&mut buf.pEvents) };

            match res {
                Ok(()) => {
                    if let Some(s) = out_sample {
                        let ts_us = unsafe { s.GetSampleTime() }
                            .map(|t| (t.max(0) as u64) / 10)
                            .unwrap_or(self.frame_counter * 33_333);
                        self.frame_counter += 1;
                        let data = extract_decoded_yuv_bytes(
                            &s,
                            self.width,
                            self.height,
                            self.stride,
                            self.output_format,
                        )?;
                        return Ok(Some(VideoFrame {
                            width: self.width,
                            height: self.height,
                            rotation: 0,
                            timestamp_us: ts_us,
                            format: self.output_format,
                            data,
                        }));
                    }
                    return Ok(None);
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // SPS/PPS changed coded resolution — re-negotiate NV12 output type and retry!
                    let (w, h, stride, fmt) = select_decoder_output_type(&self.mft)?;
                    self.width = w;
                    self.height = h;
                    self.stride = stride;
                    self.output_format = fmt;
                    if let Ok(info) = unsafe { self.mft.GetOutputStreamInfo(0) } {
                        self.mft_provides_samples = (info.dwFlags
                            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32))
                            != 0;
                    }
                    continue;
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) => {
                    return Err(VoipError::Video(format!(
                        "H.264 decoder ProcessOutput failed: {e}"
                    )));
                }
            }
        }
        Ok(None)
    }
}

fn select_decoder_output_type(mft: &IMFTransform) -> VoipResult<(u32, u32, u32, VideoPixelFormat)> {
    let mut chosen: Option<(IMFMediaType, VideoPixelFormat)> = None;
    for idx in 0..32u32 {
        // SAFETY: Enumerating decoder output media types.
        let Ok(mt) = (unsafe { mft.GetOutputAvailableType(0, idx) }) else {
            break;
        };
        let subtype = unsafe { mt.GetGUID(&MF_MT_SUBTYPE) }.unwrap_or(GUID::zeroed());
        if subtype == MFVideoFormat_NV12 {
            chosen = Some((mt, VideoPixelFormat::Nv12));
            break;
        }
        if subtype == MFVideoFormat_I420 && chosen.is_none() {
            chosen = Some((mt, VideoPixelFormat::I420));
        }
    }

    let (mt, fmt) = chosen.ok_or_else(|| {
        VoipError::Video("H.264 decoder does not advertise NV12 or I420 output".into())
    })?;

    // SAFETY: Setting selected output media type on decoder.
    unsafe { mft.SetOutputType(0, &mt, 0) }
        .map_err(|e| VoipError::Video(format!("decoder SetOutputType failed: {e}")))?;

    let (w, h) = unsafe { mt.GetUINT64(&MF_MT_FRAME_SIZE) }
        .map(unpack_u64)
        .unwrap_or((640, 360));
    let stride = unsafe { mt.GetUINT32(&MF_MT_DEFAULT_STRIDE) }
        .map(|s| (s as i32).unsigned_abs())
        .unwrap_or(w)
        .max(w);

    Ok((w, h, stride, fmt))
}

impl VideoDecoder for MfH264Decoder {
    fn codec(&self) -> VideoCodecKind {
        VideoCodecKind::H264
    }

    fn decode(&mut self, packet: &[u8]) -> VoipResult<Option<VideoFrame>> {
        if packet.is_empty() {
            return Ok(None);
        }
        let sample = create_input_sample(packet, (self.frame_counter as i64) * 333_333, 333_333)?;
        // SAFETY: Submitting Annex-B H.264 packet into decoder MFT.
        unsafe { self.mft.ProcessInput(0, &sample, 0) }
            .map_err(|e| VoipError::Video(format!("H.264 decoder ProcessInput failed: {e}")))?;

        self.pull_one_output()
    }

    fn flush(&mut self) -> VoipResult<Vec<VideoFrame>> {
        // SAFETY: Draining buffered decoded frames from decoder MFT.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
        let mut frames = Vec::new();
        while let Ok(Some(f)) = self.pull_one_output() {
            frames.push(f);
        }
        // SAFETY: Flushing decoder state cleanly.
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        Ok(frames)
    }
}

// ---------------------------------------------------------------------------
// Media Foundation Sample & ICodecAPI Helpers
// ---------------------------------------------------------------------------

fn set_codec_api_u32(api: &ICodecAPI, param: &GUID, value: u32) -> windows_core::Result<()> {
    let var = VARIANT::from(value);
    // SAFETY: Setting standard `ICodecAPI` VT_UI4 parameter.
    unsafe { api.SetValue(param, &var) }
}

fn set_codec_api_bool(api: &ICodecAPI, param: &GUID, value: bool) -> windows_core::Result<()> {
    let var = VARIANT::from(value);
    // SAFETY: Setting standard `ICodecAPI` VT_BOOL parameter.
    unsafe { api.SetValue(param, &var) }
}

fn create_input_sample(
    data: &[u8],
    timestamp_100ns: i64,
    duration_100ns: i64,
) -> VoipResult<IMFSample> {
    // SAFETY: Standard Media Foundation sample & memory buffer creation.
    let sample = unsafe { MFCreateSample() }
        .map_err(|e| VoipError::Video(format!("MFCreateSample failed: {e}")))?;
    let buffer: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(data.len() as u32) }
        .map_err(|e| VoipError::Video(format!("MFCreateMemoryBuffer failed: {e}")))?;

    let mut dst_ptr: *mut u8 = std::ptr::null_mut();
    // SAFETY: Locking freshly created buffer, copying `data`, and immediately unlocking.
    unsafe {
        buffer
            .Lock(&mut dst_ptr, None, None)
            .map_err(|e| VoipError::Video(format!("IMFMediaBuffer::Lock failed: {e}")))?;
        if !dst_ptr.is_null() && !data.is_empty() {
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst_ptr, data.len());
        }
        let _ = buffer.Unlock();
        buffer
            .SetCurrentLength(data.len() as u32)
            .map_err(|e| VoipError::Video(format!("SetCurrentLength failed: {e}")))?;
        sample
            .AddBuffer(&buffer)
            .map_err(|e| VoipError::Video(format!("AddBuffer failed: {e}")))?;
        let _ = sample.SetSampleTime(timestamp_100ns);
        let _ = sample.SetSampleDuration(duration_100ns);
    }
    Ok(sample)
}

fn create_output_sample(capacity: u32) -> VoipResult<IMFSample> {
    // SAFETY: Standard Media Foundation output sample allocation.
    let sample = unsafe { MFCreateSample() }
        .map_err(|e| VoipError::Video(format!("MFCreateSample(out) failed: {e}")))?;
    let buffer = unsafe { MFCreateMemoryBuffer(capacity) }
        .map_err(|e| VoipError::Video(format!("MFCreateMemoryBuffer(out) failed: {e}")))?;
    // SAFETY: Attaching buffer to output sample.
    unsafe { sample.AddBuffer(&buffer) }
        .map_err(|e| VoipError::Video(format!("AddBuffer(out) failed: {e}")))?;
    Ok(sample)
}

fn append_raw_sample_bytes(sample: &IMFSample, out: &mut Vec<u8>) -> VoipResult<()> {
    // SAFETY: Flattening sample buffers into a contiguous buffer and copying out.
    let buf = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(|e| VoipError::Video(format!("ConvertToContiguousBuffer failed: {e}")))?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut cur_len = 0u32;
    unsafe {
        buf.Lock(&mut ptr, None, Some(&mut cur_len))
            .map_err(|e| VoipError::Video(format!("IMFMediaBuffer::Lock failed: {e}")))?;
        if !ptr.is_null() && cur_len > 0 {
            let slice = std::slice::from_raw_parts(ptr, cur_len as usize);
            out.extend_from_slice(slice);
        }
        let _ = buf.Unlock();
    }
    Ok(())
}

fn extract_sample_bytes(
    sample: &IMFSample,
    width: u32,
    height: u32,
    format: VideoPixelFormat,
) -> VoipResult<Vec<u8>> {
    let mut raw = Vec::new();
    append_raw_sample_bytes(sample, &mut raw)?;
    let expected = match format {
        VideoPixelFormat::Nv12 | VideoPixelFormat::I420 => nv12_buffer_len(width, height),
        VideoPixelFormat::Bgra | VideoPixelFormat::Rgba => (width as usize) * (height as usize) * 4,
    };
    if raw.len() >= expected {
        raw.truncate(expected);
    }
    Ok(raw)
}

fn extract_decoded_yuv_bytes(
    sample: &IMFSample,
    width: u32,
    height: u32,
    stride: u32,
    format: VideoPixelFormat,
) -> VoipResult<Vec<u8>> {
    // SAFETY: Converting decoded sample to a contiguous buffer.
    let buf = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(|e| VoipError::Video(format!("ConvertToContiguousBuffer failed: {e}")))?;

    let w = width as usize;
    let h = height as usize;
    let expected = nv12_buffer_len(width, height);

    // Prefer `IMF2DBuffer::ContiguousCopyTo` when available so stride padding is removed cleanly.
    if let Ok(buf2d) = buf.cast::<IMF2DBuffer>() {
        let mut out = vec![0u8; expected];
        // SAFETY: `out` has exact `nv12_buffer_len(width, height)` capacity.
        let copy_res = unsafe { buf2d.ContiguousCopyTo(&mut out) };
        if copy_res.is_ok() {
            return Ok(out);
        }
    } else if buf
        .cast::<IMF2DBuffer>()
        .err()
        .is_some_and(|e| e.code() != E_NOINTERFACE)
    {
        // Continue to linear Lock fallback.
    }

    let mut raw = Vec::new();
    append_raw_sample_bytes(sample, &mut raw)?;
    let s = (stride as usize).max(w);

    if s == w || format != VideoPixelFormat::Nv12 || raw.len() < s * (h + h / 2) {
        if raw.len() >= expected {
            raw.truncate(expected);
        }
        return Ok(raw);
    }

    // De-stride NV12 manually when `stride > width`.
    let mut out = vec![0u8; expected];
    for row in 0..h {
        out[row * w..(row + 1) * w].copy_from_slice(&raw[row * s..row * s + w]);
    }
    let src_uv_base = s * h;
    let dst_uv_base = w * h;
    for row in 0..(h / 2) {
        out[dst_uv_base + row * w..dst_uv_base + (row + 1) * w]
            .copy_from_slice(&raw[src_uv_base + row * s..src_uv_base + row * s + w]);
    }
    Ok(out)
}
