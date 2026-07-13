use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssetManifest {
    #[serde(rename = "buildId")]
    pub build_id: String,
    /// Logical asset name (as referenced from templates) → `/`-prefixed hashed URL.
    pub assets: BTreeMap<String, String>,
}

impl AssetManifest {
    pub fn write(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self).context("serialising asset manifest")?;
        std::fs::write(path, body).context("writing asset-manifest.json")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_expected_json_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("asset-manifest.json");
        let mut assets = BTreeMap::new();
        assets.insert("impresspress_web.js".into(), "/impresspress_web-a1b2c3d4.js".into());
        let m = AssetManifest {
            build_id: "a1b2c3d4".into(),
            assets,
        };
        m.write(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"buildId\": \"a1b2c3d4\""));
        assert!(contents.contains("\"impresspress_web.js\": \"/impresspress_web-a1b2c3d4.js\""));
    }

    #[test]
    fn ordering_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("m.json");
        let mut assets = BTreeMap::new();
        assets.insert("z.wasm".into(), "/z.wasm".into());
        assets.insert("a.js".into(), "/a.js".into());
        let m = AssetManifest {
            build_id: "x".into(),
            assets,
        };
        m.write(&path).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        let a_pos = body.find("\"a.js\"").unwrap();
        let z_pos = body.find("\"z.wasm\"").unwrap();
        assert!(a_pos < z_pos);
    }
}
