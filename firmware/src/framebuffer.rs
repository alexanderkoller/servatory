use core::convert::Infallible;

use embedded_graphics::{
    image::ImageRaw,
    pixelcolor::{
        Rgb565,
        raw::{BigEndian, RawU16},
    },
    prelude::{DrawTarget, OriginDimensions, Pixel, Point, RawData, Size},
};

use crate::memory::{PsramBox, zeroed_psram};

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 135;
const BYTES_PER_PIXEL: usize = 2;
pub const BUFFER_SIZE: usize = WIDTH * HEIGHT * BYTES_PER_PIXEL;

/// A full-screen RGB565 framebuffer whose pixel storage lives in PSRAM.
pub struct ScreenBuffer {
    data: PsramBox<[u8]>,
}

impl ScreenBuffer {
    pub fn new() -> Self {
        Self {
            data: zeroed_psram(BUFFER_SIZE),
        }
    }

    pub fn as_image(&self) -> ImageRaw<'_, Rgb565, BigEndian> {
        ImageRaw::new(&self.data, WIDTH as u32)
    }

    fn set_pixel(&mut self, point: Point, color: Rgb565) {
        let (Ok(x), Ok(y)) = (usize::try_from(point.x), usize::try_from(point.y)) else {
            return;
        };
        if x >= WIDTH || y >= HEIGHT {
            return;
        }

        let index = (y * WIDTH + x) * BYTES_PER_PIXEL;
        self.data[index..index + BYTES_PER_PIXEL]
            .copy_from_slice(&RawU16::from(color).into_inner().to_be_bytes());
    }
}

impl DrawTarget for ScreenBuffer {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(point, color);
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        let bytes = RawU16::from(color).into_inner().to_be_bytes();
        for pixel in self.data.chunks_exact_mut(BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&bytes);
        }
        Ok(())
    }
}

impl OriginDimensions for ScreenBuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}
