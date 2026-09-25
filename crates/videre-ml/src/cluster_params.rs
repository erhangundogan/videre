//! The parameters of one face-clustering pass, shared by `videre faces`,
//! watch's repair pass, the gallery's recluster and `--evaluate`.
//!
//! There is one built-in set. watch used to pass its own literals, which
//! left the attach pass off there while `videre faces` ran it, so every
//! repair undid what the attach pass had placed.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClusteringParameters {
    /// Average-linkage cosine-distance radius (0 = identical, 2 = opposite).
    pub eps: f32,
    /// Minimum faces per cluster; smaller groups stay singletons.
    pub min_cluster_size: usize,
    /// Centroid-merge similarity: clusters at least this similar merge.
    pub merge_sim: f32,
    /// Smaller bbox side, in pixels, below which a face is held out.
    pub min_face_size: f32,
    /// Distinctiveness gate for faces without a sharpness reading.
    pub max_generic_sim: f32,
    /// Alignment gate, RMS pixels on the 112x112 ArcFace template.
    pub max_landmark_error: f32,
    /// Sharpness gate, Laplacian variance of the aligned crop.
    pub min_blur: f32,
    /// Attach pass: a leftover face joins the cluster of its nearest face
    /// when at least this similar. 1 disables the pass.
    pub attach_sim: f32,
}

impl Default for ClusteringParameters {
    fn default() -> Self {
        Self {
            eps: 0.6,
            min_cluster_size: 3,
            merge_sim: videre_core::face_cluster::DEFAULT_MERGE_SIM,
            min_face_size: videre_core::face_cluster::DEFAULT_MIN_FACE_PX,
            max_generic_sim: videre_core::face_cluster::DEFAULT_MAX_GENERIC_SIM,
            max_landmark_error: videre_core::face_cluster::DEFAULT_MAX_LANDMARK_ERR,
            min_blur: videre_core::face_cluster::DEFAULT_MIN_BLUR,
            attach_sim: videre_core::face_cluster::DEFAULT_ATTACH_SIM,
        }
    }
}

impl ClusteringParameters {
    /// The first field outside its range, by name.
    pub fn validate(&self) -> Result<(), &'static str> {
        fn in_range(value: f32, range: std::ops::RangeInclusive<f32>) -> bool {
            value.is_finite() && range.contains(&value)
        }
        if !in_range(self.eps, 0.0..=2.0) {
            return Err("eps");
        }
        if self.min_cluster_size == 0 {
            return Err("min_cluster_size");
        }
        for (value, name) in [
            (self.merge_sim, "merge_sim"),
            (self.max_generic_sim, "max_generic_sim"),
            (self.attach_sim, "attach_sim"),
        ] {
            if !in_range(value, -1.0..=1.0) {
                return Err(name);
            }
        }
        for (value, name) in [
            (self.min_face_size, "min_face_size"),
            (self.max_landmark_error, "max_landmark_error"),
            (self.min_blur, "min_blur"),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(name);
            }
        }
        Ok(())
    }
}

/// Some of the parameters: a saved override, a request, or command-line
/// flags. Fields left `None` come from whatever it is applied over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PartialClusteringParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eps: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cluster_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_sim: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_face_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_generic_sim: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_landmark_error: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_blur: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_sim: Option<f32>,
}

impl PartialClusteringParameters {
    /// Overwrite the fields this sets.
    pub fn apply_to(&self, params: &mut ClusteringParameters) {
        if let Some(v) = self.eps {
            params.eps = v;
        }
        if let Some(v) = self.min_cluster_size {
            params.min_cluster_size = v;
        }
        if let Some(v) = self.merge_sim {
            params.merge_sim = v;
        }
        if let Some(v) = self.min_face_size {
            params.min_face_size = v;
        }
        if let Some(v) = self.max_generic_sim {
            params.max_generic_sim = v;
        }
        if let Some(v) = self.max_landmark_error {
            params.max_landmark_error = v;
        }
        if let Some(v) = self.min_blur {
            params.min_blur = v;
        }
        if let Some(v) = self.attach_sim {
            params.attach_sim = v;
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The fields of `params` that differ from the built-in set: what is
    /// worth saving.
    pub fn differing_from_default(params: &ClusteringParameters) -> Self {
        let d = ClusteringParameters::default();
        let pick = |v: f32, dv: f32| (v != dv).then_some(v);
        Self {
            eps: pick(params.eps, d.eps),
            min_cluster_size: (params.min_cluster_size != d.min_cluster_size)
                .then_some(params.min_cluster_size),
            merge_sim: pick(params.merge_sim, d.merge_sim),
            min_face_size: pick(params.min_face_size, d.min_face_size),
            max_generic_sim: pick(params.max_generic_sim, d.max_generic_sim),
            max_landmark_error: pick(params.max_landmark_error, d.max_landmark_error),
            min_blur: pick(params.min_blur, d.min_blur),
            attach_sim: pick(params.attach_sim, d.attach_sim),
        }
    }

    /// `(name, value)` for every field this sets, in declaration order, with
    /// values formatted the way a person would type them.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let f = |v: f32| {
            let s = format!("{v:.2}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        let mut out = Vec::new();
        if let Some(v) = self.eps {
            out.push(("eps", f(v)));
        }
        if let Some(v) = self.min_cluster_size {
            out.push(("min_cluster_size", v.to_string()));
        }
        if let Some(v) = self.merge_sim {
            out.push(("merge_sim", f(v)));
        }
        if let Some(v) = self.min_face_size {
            out.push(("min_face_size", f(v)));
        }
        if let Some(v) = self.max_generic_sim {
            out.push(("max_generic_sim", f(v)));
        }
        if let Some(v) = self.max_landmark_error {
            out.push(("max_landmark_error", f(v)));
        }
        if let Some(v) = self.min_blur {
            out.push(("min_blur", f(v)));
        }
        if let Some(v) = self.attach_sim {
            out.push(("attach_sim", f(v)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_set_is_the_faces_command_default_with_the_attach_pass_on() {
        let d = ClusteringParameters::default();
        assert_eq!(d.eps, 0.6);
        assert_eq!(d.min_cluster_size, 3);
        assert_eq!(d.merge_sim, 0.35);
        assert_eq!(d.min_face_size, 80.0);
        assert_eq!(d.max_generic_sim, 0.4);
        assert_eq!(d.max_landmark_error, 7.0);
        assert_eq!(d.min_blur, 80.0);
        assert_eq!(d.attach_sim, 0.4);
        assert!(d.validate().is_ok());
    }

    #[test]
    fn validate_names_the_first_bad_field() {
        let bad = |change: fn(&mut ClusteringParameters)| {
            let mut p = ClusteringParameters::default();
            change(&mut p);
            p.validate().unwrap_err()
        };
        assert_eq!(bad(|p| p.eps = 2.5), "eps");
        assert_eq!(bad(|p| p.eps = f32::NAN), "eps");
        assert_eq!(bad(|p| p.min_cluster_size = 0), "min_cluster_size");
        assert_eq!(bad(|p| p.merge_sim = 1.5), "merge_sim");
        assert_eq!(bad(|p| p.max_generic_sim = -2.0), "max_generic_sim");
        assert_eq!(bad(|p| p.attach_sim = 1.01), "attach_sim");
        assert_eq!(bad(|p| p.min_face_size = -1.0), "min_face_size");
        assert_eq!(
            bad(|p| p.max_landmark_error = f32::INFINITY),
            "max_landmark_error"
        );
        assert_eq!(bad(|p| p.min_blur = -0.5), "min_blur");
    }

    #[test]
    fn a_partial_set_changes_only_its_fields() {
        let partial = PartialClusteringParameters {
            eps: Some(0.7),
            min_cluster_size: Some(2),
            ..Default::default()
        };
        let mut p = ClusteringParameters::default();
        partial.apply_to(&mut p);
        assert_eq!((p.eps, p.min_cluster_size), (0.7, 2));
        assert_eq!(p.merge_sim, ClusteringParameters::default().merge_sim);
    }

    #[test]
    fn only_fields_that_differ_from_the_default_are_kept() {
        let mut p = ClusteringParameters::default();
        assert!(PartialClusteringParameters::differing_from_default(&p).is_empty());
        p.attach_sim = 0.3;
        let diff = PartialClusteringParameters::differing_from_default(&p);
        assert_eq!(diff.fields(), vec![("attach_sim", "0.3".to_string())]);
    }

    #[test]
    fn fields_read_the_way_they_are_typed() {
        let partial = PartialClusteringParameters {
            eps: Some(0.7),
            min_cluster_size: Some(2),
            min_blur: Some(80.0),
            ..Default::default()
        };
        assert_eq!(
            partial.fields(),
            vec![
                ("eps", "0.7".to_string()),
                ("min_cluster_size", "2".to_string()),
                ("min_blur", "80".to_string()),
            ]
        );
    }
}
