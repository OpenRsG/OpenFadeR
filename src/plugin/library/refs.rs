//! Analysis refs: a track's `analysis_ref` is kept as `<plugin>:<opaque>` ([`owned_ref`]) so the plugin that made it is asked
//! again, without any library being open. A bare ref (no valid plugin name before it) was written before refs carried one: the
//! plugins that declare `legacy_analysis_refs` answer it, in registry order. A ref whose plugin is missing or switched off has
//! no analysis (callers fall back to detecting from the audio).

use super::{enabled, Loaded};
use crate::analysis::Analysis;
use crate::library::Track;
use openfader_plugin::library::split_ref;

/// What the plugin behind `track`'s `analysis_ref` holds for it.
pub fn load_ref(track: &Track) -> Option<Analysis> {
    answer(enabled(), track.analysis_ref.as_deref()?)
}

/// [`load_ref`] over `libs`.
fn answer<'a>(mut libs: impl Iterator<Item = &'a Loaded>, r: &str) -> Option<Analysis> {
    match split_ref(r) {
        Some((plugin, opaque)) => libs.find(|l| l.name() == plugin)?.analysis_ref(opaque),
        None => libs
            .filter(|l| l.decl().legacy_analysis_refs)
            .find_map(|l| l.analysis_ref(r)),
    }
}

/// Start-up migration: the store's bare `collection.analysis_ref` values go to the first of `libs` that declares
/// `legacy_analysis_refs`; nothing happens without one. Idempotent. Returns how many rows changed.
pub fn migrate_refs(libs: &[Loaded], store: &crate::store::Store) -> Result<usize, String> {
    match libs.iter().find(|l| l.decl().legacy_analysis_refs) {
        Some(l) => store.prefix_analysis_refs(l.name()),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{Manifest, Plugin};
    use openfader_plugin::library::{host::Host, Buf, LibraryApi};
    use serde_json::{json, Value};

    /// A plugin table that answers `analysis_ref` with an analysis whose `path` is the reference it got.
    unsafe extern "C" fn call(req: *const u8, len: usize, _: *const Host) -> Buf {
        let req: Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(req, len) }).unwrap();
        let reply = match req["op"].as_str() {
            Some("analysis_ref") => json!({"analysis": {"path": req["reference"]}}),
            _ => json!({"error": "unexpected"}),
        };
        let b = serde_json::to_vec(&reply).unwrap().into_boxed_slice();
        let len = b.len();
        Buf {
            ptr: Box::into_raw(b).cast(),
            len,
        }
    }

    unsafe extern "C" fn free(b: Buf) {
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(b.ptr, b.len)) });
    }

    static API: LibraryApi = LibraryApi {
        abi: openfader_plugin::library::LIBRARY_ABI,
        call,
        free,
    };

    fn lib(name: &str, legacy: bool) -> Loaded {
        let manifest: Manifest = serde_json::from_value(json!({
            "name": name,
            "library": {"lib": "x.so", "legacy_analysis_refs": legacy}
        }))
        .unwrap();
        Loaded {
            plugin: Plugin {
                dir: ".".into(),
                manifest,
            },
            api: &API,
        }
    }

    fn path(libs: &[Loaded], r: &str) -> Option<String> {
        answer(libs.iter(), r)?.path
    }

    #[test]
    fn a_ref_goes_to_the_plugin_that_made_it_with_the_opaque_part_only() {
        let libs = [lib("aa", true), lib("bb", false)];
        assert_eq!(path(&libs, "bb:/x/y:z").as_deref(), Some("/x/y:z"));
        assert_eq!(path(&libs, "aa:id").as_deref(), Some("id"));
        assert_eq!(path(&libs, "cc:id"), None); // not loaded or switched off
    }

    #[test]
    fn a_bare_ref_goes_to_the_first_legacy_plugin() {
        let libs = [lib("aa", false), lib("bb", true), lib("cc", true)];
        for bare in ["/music/a.DAT", r"C:\music\a.DAT", "rel/a.DAT"] {
            assert_eq!(path(&libs, bare).as_deref(), Some(bare), "{bare}");
        }
        assert_eq!(path(&libs[..1], "/music/a.DAT"), None); // no plugin takes bare refs
        assert_eq!(path(&[], "/music/a.DAT"), None);
    }

    #[test]
    fn the_migration_prefixes_bare_refs_once_for_the_first_legacy_plugin() {
        let store = crate::store::Store::in_memory().unwrap();
        let track = |file: &str, r: Option<&str>| Track {
            file: Some(file.into()),
            analysis_ref: r.map(str::to_owned),
            ..Track::default()
        };
        store
            .collection_put(
                &[
                    track("/a.mp3", Some("/usb/a.DAT")),
                    track("/b.mp3", Some("other:keep")),
                    track("/c.mp3", Some(r"C:\c.DAT")),
                    track("/d.mp3", None),
                ],
                false,
            )
            .unwrap();
        assert_eq!(migrate_refs(&[lib("aa", false)], &store), Ok(0)); // no legacy plugin
        let libs = [lib("aa", false), lib("bb", true), lib("cc", true)];
        assert_eq!(migrate_refs(&libs, &store), Ok(2));
        assert_eq!(migrate_refs(&libs, &store), Ok(0));
        let got: Vec<_> = store
            .collection_tracks()
            .unwrap()
            .iter()
            .map(|t| t.analysis_ref.clone())
            .collect();
        assert_eq!(
            got,
            [
                Some("bb:/usb/a.DAT".into()),
                Some("other:keep".into()),
                Some(r"bb:C:\c.DAT".into()),
                None
            ]
        );
    }
}
