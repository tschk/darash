//! Moonshine manifest types, mirroring `@tschk/moonshine-framework`'s
//! `MoonshineManifest` / `RouteArtifact` (camelCase on the wire).
//!
//! Unknown fields are ignored on purpose so older workers keep loading
//! manifests written by newer builds of the toolchain.

use serde::Deserialize;
use std::collections::BTreeMap;

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonshineManifest {
    pub version: u32,
    pub framework_version: String,
    #[serde(default)]
    pub routes: Vec<RouteArtifact>,
    #[serde(default)]
    pub assets: Vec<ManifestAsset>,
    #[serde(default)]
    pub entries: ManifestEntries,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl MoonshineManifest {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// One route in the build output. `mode` mirrors moonshine's non-"auto"
/// render modes: "static" | "ssr" | "island" | "spa" | "api".
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteArtifact {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub runtime: Option<String>,
    /// File (relative to the build output) a `static` route renders to.
    #[serde(default)]
    pub static_output: Option<String>,
    #[serde(default)]
    pub cache: Option<RouteCache>,
    #[serde(default)]
    pub headers: Option<BTreeMap<String, String>>,
}

impl RouteArtifact {
    /// Modes this crate can serve: output that already exists as a file.
    pub fn is_static_surface(&self) -> bool {
        self.mode == "static" || self.mode == "api" && self.static_output.is_some()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteCache {
    #[serde(default)]
    pub control: Option<String>,
    #[serde(default)]
    pub revalidate: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestAsset {
    pub path: String,
    pub file: String,
    #[serde(default)]
    pub integrity: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntries {
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub client: Option<String>,
}
