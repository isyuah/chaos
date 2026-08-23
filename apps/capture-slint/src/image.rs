use capture_core::capture::{CapturedFrame, PixelFormat};
use capture_core::geometry::PhysicalPoint;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::sync::Arc;

pub(super) fn placeholder_frame() -> Arc<CapturedFrame> {
    Arc::new(CapturedFrame::new(
        Arc::<[u8]>::from([0, 0, 0, 0]),
        1,
        1,
        4,
        PhysicalPoint::ZERO,
        PixelFormat::Rgba8,
    ))
}

pub(super) fn image_from_frame(frame: &CapturedFrame) -> Image {
    image_from_rgba(frame.width, frame.height, &frame.pixels)
}

pub(super) fn image_from_rgba(width: u32, height: u32, pixels: &[u8]) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    let target = buffer.make_mut_slice();
    for (pixel, rgba) in target.iter_mut().zip(pixels.as_chunks::<4>().0.iter()) {
        *pixel = Rgba8Pixel {
            r: rgba[0],
            g: rgba[1],
            b: rgba[2],
            a: rgba[3],
        };
    }
    Image::from_rgba8(buffer)
}
