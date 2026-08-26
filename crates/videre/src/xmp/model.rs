//! The typed set of properties videre owns in an XMP sidecar, plus the pixel
//! to normalized-Area conversion the MWG region format needs. Pure data and
//! arithmetic; no I/O, no database, so it is trivially testable.

/// One MWG region area, center-based and normalized to 0..1, as MWG requires.
#[derive(Debug, Clone, PartialEq)]
pub struct Area {
    pub cx: f64,
    pub cy: f64,
    pub w: f64,
    pub h: f64,
}

impl Area {
    /// Convert a stored `"x,y,w,h"` top-left pixel bbox to a centered unit Area,
    /// given the image pixel dimensions. Returns None on a malformed bbox or a
    /// zero dimension (which would divide by zero and cannot be a real image).
    pub fn from_pixel_bbox(bbox: &str, img_w: u32, img_h: u32) -> Option<Area> {
        if img_w == 0 || img_h == 0 {
            return None;
        }
        let nums: Vec<f64> = bbox
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        let [x, y, w, h] = nums.as_slice() else {
            return None;
        };
        let (iw, ih) = (img_w as f64, img_h as f64);
        Some(Area {
            cx: (x + w / 2.0) / iw,
            cy: (y + h / 2.0) / ih,
            w: w / iw,
            h: h / ih,
        })
    }
}

/// One named face region for a photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub name: String,
    pub area: Area,
}

/// Every property videre owns for one photo. Empty everywhere means "nothing to
/// write"; the writer skips such a photo.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwnedXmp {
    pub rating: Option<i64>,
    pub label: Option<String>,
    pub location: Option<String>,
    pub keywords: Vec<String>,
    pub regions: Vec<Region>,
    /// Pixel dimensions the regions were computed against (MWG AppliedToDimensions).
    pub applied_dims: Option<(u32, u32)>,
}

impl OwnedXmp {
    pub fn is_empty(&self) -> bool {
        self.rating.is_none()
            && self.label.is_none()
            && self.location.is_none()
            && self.keywords.is_empty()
            && self.regions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_top_left_pixel_bbox_to_centered_unit_area() {
        // 4000x3000 image, face box top-left (1000,600) size 800x900.
        let a = Area::from_pixel_bbox("1000,600,800,900", 4000, 3000).unwrap();
        // center x = (1000+400)/4000 = 0.35 ; center y = (600+450)/3000 = 0.35
        assert!((a.cx - 0.35).abs() < 1e-6);
        assert!((a.cy - 0.35).abs() < 1e-6);
        assert!((a.w - 0.20).abs() < 1e-6); // 800/4000
        assert!((a.h - 0.30).abs() < 1e-6); // 900/3000
    }

    #[test]
    fn rejects_bad_bbox_or_zero_dimension() {
        assert!(Area::from_pixel_bbox("nope", 4000, 3000).is_none());
        assert!(Area::from_pixel_bbox("0,0,10,10", 0, 3000).is_none());
    }

    #[test]
    fn owned_is_empty_only_when_all_fields_absent() {
        assert!(OwnedXmp::default().is_empty());
        assert!(!OwnedXmp {
            rating: Some(1),
            ..Default::default()
        }
        .is_empty());
    }
}
