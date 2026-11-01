/// Axis-aligned rectangle in pixel coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub fn new(x: usize, y: usize, width: usize, height: usize) -> Self {
        Self { x, y, width, height }
    }
}

/// Single-channel `f32` image, e.g. a CFA mosaic, luminance or a mask.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Plane {
    pub fn new(width: usize, height: usize) -> Self {
        Self::from_vec(width, height, vec![0.0; width * height])
    }

    pub fn from_vec(width: usize, height: usize, data: Vec<f32>) -> Self {
        assert_eq!(data.len(), width * height, "plane data size mismatch");
        Self { width, height, data }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    pub fn set(&mut self, x: usize, y: usize, value: f32) {
        self.data[y * self.width + x] = value;
    }

    pub fn data(&self) -> &[f32] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }
}

/// Interleaved linear RGB image in camera colour space.
///
/// Values are normalised so that 0.0 is the sensor black level and 1.0 the white level.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbImage {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl RgbImage {
    pub fn new(width: usize, height: usize) -> Self {
        Self::from_vec(width, height, vec![0.0; width * height * 3])
    }

    pub fn from_vec(width: usize, height: usize, data: Vec<f32>) -> Self {
        assert_eq!(data.len(), width * height * 3, "RGB data size mismatch");
        Self { width, height, data }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pixel(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, rgb: [f32; 3]) {
        let i = (y * self.width + x) * 3;
        self.data[i..i + 3].copy_from_slice(&rgb);
    }

    pub fn data(&self) -> &[f32] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Copies the given region into a new image. Panics if `rect` exceeds the image.
    pub fn crop(&self, rect: Rect) -> RgbImage {
        assert!(rect.x + rect.width <= self.width && rect.y + rect.height <= self.height);
        let mut data = Vec::with_capacity(rect.width * rect.height * 3);
        for y in rect.y..rect.y + rect.height {
            let start = (y * self.width + rect.x) * 3;
            data.extend_from_slice(&self.data[start..start + rect.width * 3]);
        }
        RgbImage::from_vec(rect.width, rect.height, data)
    }
}

/// 8-bit sRGB image for display, RGB interleaved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayImage {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_copies_region() {
        let mut img = RgbImage::new(4, 3);
        for y in 0..3 {
            for x in 0..4 {
                img.set_pixel(x, y, [x as f32, y as f32, 0.0]);
            }
        }
        let c = img.crop(Rect::new(1, 1, 2, 2));
        assert_eq!((c.width(), c.height()), (2, 2));
        assert_eq!(c.pixel(0, 0), [1.0, 1.0, 0.0]);
        assert_eq!(c.pixel(1, 1), [2.0, 2.0, 0.0]);
    }

    #[test]
    #[should_panic]
    fn crop_out_of_bounds_panics() {
        RgbImage::new(4, 3).crop(Rect::new(3, 0, 2, 1));
    }
}
