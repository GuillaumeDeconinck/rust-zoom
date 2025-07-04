/*
 * Copyright 2025 Security Union LLC
 *
 * Licensed under either of
 *
 * * Apache License, Version 2.0
 *   (http://www.apache.org/licenses/LICENSE-2.0)
 * * MIT license
 *   (http://opensource.org/licenses/MIT)
 *
 * at your option.
 *
 * Unless you explicitly state otherwise, any contribution intentionally
 * submitted for inclusion in the work by you, as defined in the Apache-2.0
 * license, shall be dual licensed as above, without any additional terms or
 * conditions.
 */



#![allow(clippy::too_many_arguments)]

use crate::{
    ApiBackend, CameraControl, CameraFormat, CameraInfo, CaptureBackendTrait, FrameFormat,
    KnownCameraControl, KnownCameraControlFlag, NokhwaError, Resolution,
};
use flume::{Receiver, Sender};
use image::{ImageBuffer, Rgb};
use ouroboros::self_referencing;
use std::{
    any::Any,
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::HashMap,
    mem::MaybeUninit,
    sync::{atomic::AtomicUsize, Arc},
};
use uvc::{
    ActiveStream, Context, DescriptionSubtype, Device, DeviceHandle, StreamFormat, StreamHandle,
};

// ignore the IDE, this compiles
/// The backend struct that interfaces with `libuvc`.
/// To see what this does, please see [`CaptureBackendTrait`]
/// # Quirks
/// - You may need administrator/superuser privileges to access a UVC device.
/// - The indexing for this backend is based off of `libuvc`'s device ordering, not the OS.
/// - You must call [create()](UVCCaptureDevice::create()) instead `new()`, some methods are auto-generated by the self-referencer and are not meant to be used.
/// - The [create()](UVCCaptureDevice::create()) method will open the device twice.
/// - Calling [`set_resolution()`](CaptureBackendTrait::set_resolution()), [`set_frame_rate()`](crate::CaptureBackendTrait::set_frame_rate()), or [`set_frame_format()`](crate::CaptureBackendTrait::set_frame_format()) each internally calls [`set_camera_format()`](crate::CaptureBackendTrait::set_camera_format()).
/// - [`frame_raw()`](crate::CaptureBackendTrait::frame_raw()) returns the same raw data as [`get_frame()`](crate::CaptureBackendTrait::frame()), a.k.a. no custom decoding required, all data is automatically RGB
/// - The [`frame_raw()`](crate::CaptureBackendTrait::frame_raw()) and by extension [`frame()`](crate::CaptureBackendTrait::frame()) functions block.
/// - Setting controls is not supported.
/// - This backend, once stream is open, will constantly collect frames. When you call [`frame()`](crate::CaptureBackendTrait::frame()) or one of its variants, it will only give you the latest frame.
/// # Safety
/// This backend requires use of `unsafe` due to the self-referencing structs involved.
/// - If [`open_stream()`](crate::CaptureBackendTrait::open_stream()) and [`frame()`](crate::CaptureBackendTrait::frame()) are called in the wrong order this will cause undefined behaviour.
/// - If internal variables `stream_handle_init` and `active_stream_init` become de-synchronized with the true reality (weather streamhandle/activestream is init or not) this will cause undefined behaviour.
#[self_referencing]
#[cfg_attr(feature = "docs-features", doc(cfg(feature = "input-uvc")))]
#[deprecated(
    since = "0.10",
    note = "Use one of the native backends instead(V4L, AVF, MSMF) or OpenCV"
)]
pub struct UVCCaptureDevice<'a> {
    camera_format: CameraFormat,
    camera_info: CameraInfo<'a>,
    frame_receiver: Receiver<Vec<u8>>,
    frame_sender: Sender<Vec<u8>>,
    stream_handle_init: Cell<bool>,
    active_stream_init: Cell<bool>,
    context: Context<'a>,
    #[not_covariant]
    #[borrows(context)]
    device: Device<'this>,
    #[not_covariant]
    #[borrows(device)]
    device_handle: DeviceHandle<'this>,
    stream_handle: RefCell<MaybeUninit<StreamHandle<'a>>>,
    active_stream: RefCell<MaybeUninit<ActiveStream<'a, Arc<AtomicUsize>>>>,
}

impl<'a> UVCCaptureDevice<'a> {
    /// Creates a UVC Camera device with optional [`CameraFormat`].
    /// If `camera_format` is `None`, it will be spawned with with 640x480@15 FPS, MJPEG [`CameraFormat`] default.
    /// # Panics
    /// This operation may panic! If the UVC Context fails to retrieve the device from the gotten IDs, this operation will panic.
    /// # Errors
    /// This may error when the `libuvc` backend fails to retrieve the device or its data.
    pub fn create(index: usize, cam_fmt: Option<CameraFormat>) -> Result<Self, NokhwaError> {
        let context = match Context::new() {
            Ok(ctx) => ctx,
            Err(why) => {
                return Err(NokhwaError::OpenDeviceError(
                    index.to_string(),
                    why.to_string(),
                ))
            }
        };

        let (camera_info, frame_receiver, frame_sender) = {
            let device_list = match context.devices() {
                Ok(device_list) => device_list,
                Err(why) => {
                    return Err(NokhwaError::OpenDeviceError(
                        index.to_string(),
                        why.to_string(),
                    ))
                }
            };

            let device = match device_list.into_iter().nth(index) {
                Some(d) => d,
                None => {
                    return Err(NokhwaError::OpenDeviceError(
                        index.to_string(),
                        "Not Found".to_string(),
                    ))
                }
            };

            let device_desc = match device.description() {
                Ok(desc) => desc,
                Err(why) => {
                    return Err(NokhwaError::OpenDeviceError(
                        index.to_string(),
                        why.to_string(),
                    ))
                }
            };

            let device_name = match (device_desc.manufacturer, device_desc.product) {
                (Some(manu), Some(prod)) => {
                    format!("{} {}", manu, prod)
                }
                (_, Some(prod)) => prod,
                (Some(manu), _) => {
                    format!(
                        "{}:{} {}",
                        device_desc.vendor_id, device_desc.product_id, manu
                    )
                }
                (_, _) => {
                    format!("{}:{}", device_desc.vendor_id, device_desc.product_id)
                }
            };

            let camera_info = CameraInfo::new(
                device_name,
                "".to_string(),
                format!("{}:{}", device_desc.vendor_id, device_desc.product_id),
                index,
            );

            let (frame_sender, frame_receiver) = {
                let (a, b) = flume::unbounded::<Vec<u8>>();
                (a, b)
            };
            (camera_info, frame_receiver, frame_sender)
        };

        let camera_format = match cam_fmt {
            Some(cfmt) => cfmt,
            None => CameraFormat::default(),
        };

        Ok(UVCCaptureDeviceBuilder {
            camera_format,
            camera_info,
            frame_receiver,
            frame_sender,
            context,
            stream_handle_init: Cell::new(false),
            active_stream_init: Cell::new(false),
            device_builder: |context_builder| {
                context_builder
                    .devices()
                    .unwrap()
                    .into_iter()
                    .nth(index)
                    .unwrap()
            },
            device_handle_builder: |device_builder| device_builder.open().unwrap(),
            stream_handle: RefCell::new(MaybeUninit::uninit()),
            active_stream: RefCell::new(MaybeUninit::uninit()),
        }
        .build())
    }

    /// Create a UVC Camera with desired settings.
    /// # Panics
    /// This operation may panic! If the UVC Context fails to retrieve the device from the gotten IDs, this operation will panic.
    /// # Errors
    /// This may error when the `libuvc` backend fails to retrieve the device or its data.
    pub fn create_with(
        index: usize,
        width: u32,
        height: u32,
        fps: u32,
        fourcc: FrameFormat,
    ) -> Result<Self, NokhwaError> {
        let camera_format = Some(CameraFormat::new_from(width, height, fourcc, fps));
        UVCCaptureDevice::create(index, camera_format)
    }
}

// IDE Autocomplete ends here. Do not be afraid it your IDE does not show completion.
// Here are some docs to help you out: https://docs.rs/ouroboros/0.9.3/ouroboros/attr.self_referencing.html
impl<'a> CaptureBackendTrait for UVCCaptureDevice<'a> {
    fn backend(&self) -> ApiBackend {
        ApiBackend::UniversalVideoClass
    }

    fn camera_info(&self) -> &CameraInfo {
        self.borrow_camera_info()
    }

    fn camera_format(&self) -> CameraFormat {
        *self.borrow_camera_format()
    }

    fn set_camera_format(&mut self, new_fmt: CameraFormat) -> Result<(), NokhwaError> {
        let prev_fmt = *self.borrow_camera_format();

        self.with_camera_format_mut(|cfmt| {
            *cfmt = new_fmt;
        });

        let is_streamh_some = self.borrow_stream_handle_init().get();

        if is_streamh_some {
            return match self.open_stream() {
                Ok(_) => Ok(()),
                Err(why) => {
                    // revert
                    self.with_camera_format_mut(|cfmt| {
                        *cfmt = prev_fmt;
                    });
                    Err(NokhwaError::SetPropertyError {
                        property: "CameraFormat".to_string(),
                        value: new_fmt.to_string(),
                        error: why.to_string(),
                    })
                }
            };
        }
        Ok(())
    }

    #[allow(clippy::cast_possible_truncation)]
    fn compatible_list_by_resolution(
        &mut self,
        fourcc: FrameFormat,
    ) -> Result<HashMap<Resolution, Vec<u32>>, NokhwaError> {
        let mut resolution_fps_map: HashMap<Resolution, Vec<u32>> = HashMap::new();
        for fmt in self.with_device_handle(|devh| devh).supported_formats() {
            for frame_desc in fmt.supported_formats() {
                // FIXME: Verify that this is correct way to interpret DescriptionSubtype!
                let format = match frame_desc.subtype() {
                    DescriptionSubtype::FormatMJPEG | DescriptionSubtype::FrameMJPEG => {
                        FrameFormat::MJPEG
                    }
                    DescriptionSubtype::FormatUncompressed
                    | DescriptionSubtype::FrameUncompressed => FrameFormat::YUYV,
                    _ => continue,
                };

                if format != fourcc {
                    continue;
                }

                let resolution =
                    Resolution::new(frame_desc.width().into(), frame_desc.height().into());
                let fps: Vec<u32> = frame_desc
                    .intervals_duration()
                    .into_iter()
                    .map(|duration| (1000 / duration.as_millis()) as u32)
                    .collect();
                resolution_fps_map.insert(resolution, fps);
            }
        }
        Ok(resolution_fps_map)
    }

    fn compatible_fourcc(&mut self) -> Result<Vec<FrameFormat>, NokhwaError> {
        let mut frameformats = vec![];
        for fmt in self.with_device_handle(|devh| devh).supported_formats() {
            for frame_desc in fmt.supported_formats() {
                // FIXME: Verify that this is correct way to interpret DescriptionSubtype!
                match frame_desc.subtype() {
                    DescriptionSubtype::FormatMJPEG | DescriptionSubtype::FrameMJPEG => {
                        frameformats.push(FrameFormat::MJPEG);
                    }
                    DescriptionSubtype::FormatUncompressed
                    | DescriptionSubtype::FrameUncompressed => frameformats.push(FrameFormat::YUYV),
                    _ => continue,
                };
            }
        }

        frameformats.sort();
        frameformats.dedup();
        Ok(frameformats)
    }

    fn resolution(&self) -> Resolution {
        self.borrow_camera_format().resolution()
    }

    fn set_resolution(&mut self, new_res: Resolution) -> Result<(), NokhwaError> {
        let mut current_format = *self.borrow_camera_format();
        current_format.set_resolution(new_res);
        self.set_camera_format(current_format)
    }

    fn frame_rate(&self) -> u32 {
        self.borrow_camera_format().frame_rate()
    }

    fn set_frame_rate(&mut self, new_fps: u32) -> Result<(), NokhwaError> {
        let mut current_format = *self.borrow_camera_format();
        current_format.set_frame_rate(new_fps);
        self.set_camera_format(current_format)
    }

    fn frame_format(&self) -> FrameFormat {
        self.borrow_camera_format().format()
    }

    fn set_frame_format(&mut self, fourcc: FrameFormat) -> Result<(), NokhwaError> {
        let mut current_format = *self.borrow_camera_format();
        current_format.set_format(fourcc);
        self.set_camera_format(current_format)
    }

    fn supported_camera_controls(&self) -> Result<Vec<KnownCameraControl>, NokhwaError> {
        Ok(vec![
            KnownCameraControl::Exposure,
            KnownCameraControl::Focus,
        ])
    }

    fn camera_control(&self, control: KnownCameraControl) -> Result<CameraControl, NokhwaError> {
        match control {
            KnownCameraControl::Focus => match self.with_device_handle(|x| x).exposure_rel() {
                Ok(v) => {
                    let v: i8 = v;
                    match CameraControl::new(
                        control,
                        i32::from(i8::MIN),
                        i32::from(i8::MAX),
                        i32::from(v),
                        1_i32,
                        i32::from(v),
                        KnownCameraControlFlag::Automatic,
                        true,
                    ) {
                        Ok(cc) => Ok(cc),
                        Err(why) => Err(NokhwaError::GetPropertyError {
                            property: control.to_string(),
                            error: why.to_string(),
                        }),
                    }
                }
                Err(why) => Err(NokhwaError::GetPropertyError {
                    property: control.to_string(),
                    error: why.to_string(),
                }),
            },
            _ => Err(NokhwaError::GetPropertyError {
                property: control.to_string(),
                error: "Not Supported".to_string(),
            }),
        }
    }

    fn set_camera_control(&mut self, _control: CameraControl) -> Result<(), NokhwaError> {
        Err(NokhwaError::UnsupportedOperationError(
            ApiBackend::UniversalVideoClass,
        ))
    }

    fn raw_supported_camera_controls(&self) -> Result<Vec<Box<dyn Any>>, NokhwaError> {
        Err(NokhwaError::UnsupportedOperationError(
            ApiBackend::UniversalVideoClass,
        ))
    }

    fn raw_camera_control(&self, _control: &dyn Any) -> Result<Box<dyn Any>, NokhwaError> {
        Err(NokhwaError::UnsupportedOperationError(
            ApiBackend::UniversalVideoClass,
        ))
    }

    fn set_raw_camera_control(
        &mut self,
        _control: &dyn Any,
        _value: &dyn Any,
    ) -> Result<(), NokhwaError> {
        Err(NokhwaError::UnsupportedOperationError(
            ApiBackend::UniversalVideoClass,
        ))
    }

    fn open_stream(&mut self) -> Result<(), NokhwaError> {
        let ret: Result<(), NokhwaError> = self.with_mut(|fields| {
            let stream_format: StreamFormat = StreamFormat {
                width: (*fields.camera_format).width(),
                height: (*fields.camera_format).height(),
                fps: (*fields.camera_format).frame_rate(),
                format: (*fields.camera_format).format().into(),
            };

            // first, drop the existing stream by setting it to None
            {
                if fields.active_stream_init.get() {
                    let innard_value = fields.active_stream.replace(MaybeUninit::uninit());
                    unsafe {
                        std::mem::drop(innard_value.assume_init());
                    };
                    fields.active_stream_init.set(false);
                }

                if fields.stream_handle_init.get() {
                    let innard_value = fields.stream_handle.replace(MaybeUninit::uninit());
                    unsafe {
                        std::mem::drop(innard_value.assume_init());
                    };
                    fields.stream_handle_init.set(false);
                }
            }
            // second, set the stream handle according to the streamformat
            match fields
                .device_handle
                .get_stream_handle_with_format(stream_format)
            {
                Ok(streamh) => match fields.stream_handle.try_borrow_mut() {
                    Ok(mut streamh_raw) => {
                        *streamh_raw = MaybeUninit::new(streamh);
                        fields.stream_handle_init.set(true);
                    }
                    Err(why) => return Err(NokhwaError::OpenStreamError(why.to_string())),
                },
                Err(why) => return Err(NokhwaError::OpenStreamError(why.to_string())),
            }
            Ok(())
        });

        if ret.is_err() {
            return ret;
        }

        let ret_2: Result<(), NokhwaError> = self.with(|fields| {
            // finally, get the active stream
            let counter = Arc::new(AtomicUsize::new(0));
            let frame_sender: Sender<Vec<u8>> = self.with_frame_sender(Clone::clone);
            let streamh = unsafe {
                let raw_ptr =
                    (*fields.stream_handle.borrow_mut()).as_ptr() as *mut MaybeUninit<StreamHandle>;
                let assume_inited: *mut MaybeUninit<StreamHandle<'static>> =
                    raw_ptr.cast::<MaybeUninit<uvc::StreamHandle>>();
                &mut *assume_inited
            };
            let streamh_init = unsafe {
                match streamh.as_mut_ptr().as_mut() {
                    Some(sth) => sth,
                    None => {
                        return Err(NokhwaError::OpenStreamError(
                            "Failed to get mutable raw pointer to stream handle!".to_string(),
                        ))
                    }
                }
            };

            let active_stream = match streamh_init.start_stream(
                move |frame, _count| {
                    let vec_frame = frame.to_rgb().unwrap().to_bytes().to_vec();
                    if frame_sender.send(vec_frame).is_err() {
                        // do nothing
                    }
                },
                counter,
            ) {
                Ok(active) => active,
                Err(why) => return Err(NokhwaError::OpenStreamError(why.to_string())),
            };
            *fields.active_stream.borrow_mut() = MaybeUninit::new(active_stream);
            Ok(())
        });

        if ret_2.is_err() {
            return ret_2;
        }
        self.borrow_active_stream_init().set(true);

        Ok(())
    }

    fn is_stream_open(&self) -> bool {
        self.with_active_stream_init(Cell::get)
    }

    fn frame(&mut self) -> Result<ImageBuffer<Rgb<u8>, Vec<u8>>, NokhwaError> {
        let resolution: Resolution = self.borrow_camera_format().resolution();

        let data = match self.frame_raw() {
            Ok(d) => d,
            Err(why) => return Err(why),
        };

        let imagebuf: ImageBuffer<Rgb<u8>, Vec<u8>> =
            match ImageBuffer::from_vec(resolution.width(), resolution.height(), data.to_vec()) {
                Some(img) => img,
                None => {
                    return Err(NokhwaError::ReadFrameError(
                        "ImageBuffer too small! This is probably a bug, please report it!"
                            .to_string(),
                    ))
                }
            };

        Ok(imagebuf)
    }

    fn frame_raw(&mut self) -> Result<Cow<[u8]>, NokhwaError> {
        // assertions
        if !self.borrow_active_stream_init().get() {
            return Err(NokhwaError::ReadFrameError(
                "Please call `open_stream()` first!".to_string(),
            ));
        }

        let f_recv = self.borrow_frame_receiver();
        let messages_iter = f_recv.drain();
        match messages_iter.last() {
            Some(msg) => Ok(Cow::from(msg)),
            None => match f_recv.recv() {
                Ok(msg) => Ok(Cow::from(msg)),
                Err(why) => {
                    return Err(NokhwaError::ReadFrameError(format!(
                        "All sender dropped: {}",
                        why.to_string()
                    )))
                }
            },
        }
    }

    fn stop_stream(&mut self) -> Result<(), NokhwaError> {
        self.with(|fields| {
            if fields.active_stream_init.get() {
                let innard_value = fields.active_stream.replace(MaybeUninit::uninit());
                unsafe {
                    std::mem::drop(innard_value.assume_init());
                };
                fields.active_stream_init.set(false);
            }

            if fields.stream_handle_init.get() {
                let innard_value = fields.stream_handle.replace(MaybeUninit::uninit());
                unsafe {
                    std::mem::drop(innard_value.assume_init());
                };
                fields.stream_handle_init.set(false);
            }
        });
        Ok(())
    }
}
