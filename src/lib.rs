//! OpenFadeR core: everything except the Slint UI (which lives in the binary, `src/app/`).
pub mod analysis;
pub mod audio;
pub mod catalog;
#[cfg(not(target_os = "android"))]
pub mod device;
pub mod edit;
pub mod library;
pub mod lighting;
pub mod paths;
pub mod plugin;
#[cfg(feature = "stems")]
pub mod stems;
pub mod store;
pub mod video;
pub mod zip;
