pub const RED: u8 = 0;
pub const GREEN: u8 = 1;
pub const BLUE: u8 = 2;

/// 2×2 Bayer colour filter pattern; `rows[y][x]` is the colour of the photosite at
/// `(x mod 2, y mod 2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CfaPattern {
    rows: [[u8; 2]; 2],
}

impl CfaPattern {
    pub const RGGB: Self = Self {
        rows: [[RED, GREEN], [GREEN, BLUE]],
    };

    /// Parses a pattern name such as `"RGGB"` or `"GBRG"`. Only Bayer patterns (one red,
    /// one blue, two greens) are accepted.
    pub fn from_name(name: &str) -> Option<Self> {
        let colors: Vec<u8> = name
            .chars()
            .map(|c| match c {
                'R' => Some(RED),
                'G' => Some(GREEN),
                'B' => Some(BLUE),
                _ => None,
            })
            .collect::<Option<_>>()?;
        let [a, b, c, d] = colors[..] else { return None };
        let pattern = Self { rows: [[a, b], [c, d]] };
        pattern.is_bayer().then_some(pattern)
    }

    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.rows[y & 1][x & 1]
    }

    /// The pattern as seen from an image cropped at `(dx, dy)`.
    pub fn shifted(&self, dx: usize, dy: usize) -> Self {
        let mut rows = [[0; 2]; 2];
        for (y, row) in rows.iter_mut().enumerate() {
            for (x, c) in row.iter_mut().enumerate() {
                *c = self.color_at(x + dx, y + dy);
            }
        }
        Self { rows }
    }

    fn is_bayer(&self) -> bool {
        let [[a, b], [c, d]] = self.rows;
        let (greens, others) = if a == GREEN && d == GREEN {
            ((a, d), (b, c))
        } else {
            ((b, c), (a, d))
        };
        greens == (GREEN, GREEN) && (others == (RED, BLUE) || others == (BLUE, RED))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bayer_patterns() {
        assert_eq!(CfaPattern::from_name("RGGB"), Some(CfaPattern::RGGB));
        let p = CfaPattern::from_name("GBRG").unwrap();
        assert_eq!(p.color_at(0, 0), GREEN);
        assert_eq!(p.color_at(1, 0), BLUE);
        assert_eq!(p.color_at(0, 1), RED);
        assert_eq!(p.color_at(3, 3), GREEN);
    }

    #[test]
    fn rejects_non_bayer() {
        assert_eq!(CfaPattern::from_name("RGBG"), None); // greens not diagonal
        assert_eq!(CfaPattern::from_name("RRGB"), None);
        assert_eq!(CfaPattern::from_name("RGGBX"), None);
        assert_eq!(CfaPattern::from_name("CYGM"), None);
    }

    #[test]
    fn shift_moves_origin() {
        let p = CfaPattern::RGGB;
        assert_eq!(p.shifted(1, 0), CfaPattern::from_name("GRBG").unwrap());
        assert_eq!(p.shifted(0, 1), CfaPattern::from_name("GBRG").unwrap());
        assert_eq!(p.shifted(1, 1), CfaPattern::from_name("BGGR").unwrap());
        assert_eq!(p.shifted(144, 72), p);
    }
}
