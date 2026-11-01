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

/// How an image is shown: the stored (sensor) image rotated clockwise by a multiple of
/// 90°. Processing always happens in sensor orientation; this only affects display and
/// the orientation tag of saved files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Orientation {
    #[default]
    Normal,
    Cw90,
    Rotate180,
    Cw270,
}

impl Orientation {
    /// From the EXIF/TIFF orientation tag. Mirrored variants (never written by cameras)
    /// are treated as their rotation part.
    pub fn from_exif(value: u16) -> Self {
        match value {
            3 | 4 => Self::Rotate180,
            6 | 5 => Self::Cw90,
            8 | 7 => Self::Cw270,
            _ => Self::Normal,
        }
    }

    pub fn to_exif(self) -> u16 {
        match self {
            Self::Normal => 1,
            Self::Cw90 => 6,
            Self::Rotate180 => 3,
            Self::Cw270 => 8,
        }
    }

    fn quarter_turns(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Cw90 => 1,
            Self::Rotate180 => 2,
            Self::Cw270 => 3,
        }
    }

    fn from_quarter_turns(n: u8) -> Self {
        [Self::Normal, Self::Cw90, Self::Rotate180, Self::Cw270][(n % 4) as usize]
    }

    pub fn rotated_cw(self) -> Self {
        Self::from_quarter_turns(self.quarter_turns() + 1)
    }

    pub fn rotated_ccw(self) -> Self {
        Self::from_quarter_turns(self.quarter_turns() + 3)
    }

    /// Clockwise rotation in degrees.
    pub fn degrees(self) -> u32 {
        90 * self.quarter_turns() as u32
    }

    /// Size of the displayed image for a stored image of `width × height`.
    pub fn display_size(self, width: f32, height: f32) -> (f32, f32) {
        if self.quarter_turns() % 2 == 1 { (height, width) } else { (width, height) }
    }

    /// Maps a position in the stored image (`size` = its width and height) to the display.
    pub fn to_display(self, (x, y): (f32, f32), (w, h): (f32, f32)) -> (f32, f32) {
        match self {
            Self::Normal => (x, y),
            Self::Cw90 => (h - y, x),
            Self::Rotate180 => (w - x, h - y),
            Self::Cw270 => (y, w - x),
        }
    }

    /// Inverse of [`Self::to_display`]; `size` is the stored image's width and height.
    pub fn to_image(self, (u, v): (f32, f32), (w, h): (f32, f32)) -> (f32, f32) {
        match self {
            Self::Normal => (u, v),
            Self::Cw90 => (v, h - u),
            Self::Rotate180 => (w - u, h - v),
            Self::Cw270 => (w - v, u),
        }
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
    fn orientation_exif_roundtrip_and_turns() {
        for o in [Orientation::Normal, Orientation::Cw90, Orientation::Rotate180, Orientation::Cw270] {
            assert_eq!(Orientation::from_exif(o.to_exif()), o);
            assert_eq!(o.rotated_cw().rotated_ccw(), o);
        }
        assert_eq!(Orientation::Normal.rotated_ccw(), Orientation::Cw270);
        assert_eq!(Orientation::Cw270.rotated_cw(), Orientation::Normal);
        assert_eq!(Orientation::from_exif(0), Orientation::Normal);
        assert_eq!(Orientation::Cw90.degrees(), 90);
    }

    #[test]
    fn orientation_maps_corners_and_roundtrips() {
        let size = (40.0, 30.0);
        // The stored image's top-left corner ends up …
        let expected = [
            (Orientation::Normal, (0.0, 0.0)),
            (Orientation::Cw90, (30.0, 0.0)),       // … top-right after turning right
            (Orientation::Rotate180, (40.0, 30.0)), // … bottom-right
            (Orientation::Cw270, (0.0, 40.0)),      // … bottom-left after turning left
        ];
        for (o, corner) in expected {
            assert_eq!(o.to_display((0.0, 0.0), size), corner, "{o:?}");
            let (dw, dh) = o.display_size(size.0, size.1);
            let p = (13.0, 7.5);
            let d = o.to_display(p, size);
            assert!(d.0 >= 0.0 && d.0 <= dw && d.1 >= 0.0 && d.1 <= dh);
            assert_eq!(o.to_image(d, size), p, "{o:?}");
        }
    }

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
