use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::calibration::sha256_hex;
use crate::episode::MujocoRuntimeIdentity;
use piper_physics::PhysicsError;
use thiserror::Error;

const MODEL_HASH_DOMAIN: &[u8] = b"PIPER_MUJOCO_MODEL_HASH_V1\n";
const EMBEDDED_MODEL_HASH_DOMAIN: &[u8] = b"PIPER_MUJOCO_EMBEDDED_MODEL_HASH_V1\n";
const HASH_ALGORITHM: &str = "sha256";
const EMBEDDED_ROOT_XML: &str = "embedded.xml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MujocoModelHash {
    pub hash_algorithm: String,
    pub sha256_hex: String,
    pub root_xml_relative_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum ModelHashError {
    #[error("MuJoCo model directory is not a directory: {path}")]
    InvalidModelDirectory { path: PathBuf },
    #[error("MuJoCo model path must be UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
    #[error("MuJoCo model path is not a canonical relative path: {path}")]
    InvalidRelativePath { path: String },
    #[error("MuJoCo model tree contains a symlink: {path}")]
    Symlink { path: PathBuf },
    #[error("MuJoCo model tree contains a non-regular file: {path}")]
    NonRegularFile { path: PathBuf },
    #[error("MuJoCo model tree contains a duplicate normalized path: {path}")]
    DuplicateNormalizedPath { path: String },
    #[error("MuJoCo root XML is not in the hashed model tree: {path}")]
    RootXmlNotInModelTree { path: String },
    #[error("MuJoCo XML file is not UTF-8: {path}")]
    XmlNotUtf8 { path: String },
    #[error("invalid MuJoCo XML in {path}: {message}")]
    InvalidXml { path: String, message: String },
    #[error("embedded MuJoCo model contains external file reference: file={reference}")]
    EmbeddedExternalFileReference { reference: String },
    #[error("MuJoCo XML file reference escapes hashed model tree in {xml_path}: file={reference}")]
    EscapingFileReference { xml_path: String, reference: String },
    #[error(
        "MuJoCo XML file reference cannot be proven inside hashed tree in {xml_path}: file={reference}"
    )]
    UnresolvedFileReference { xml_path: String, reference: String },
    #[error(
        "MuJoCo XML attribute `{attribute}` in {xml_path} changes asset resolution and is not supported by the v1 model hash"
    )]
    UnsupportedAssetResolutionAttribute { xml_path: String, attribute: String },
    #[error(
        "MuJoCo runtime identity unavailable before MIT enable for runtime {version}: {source}"
    )]
    RuntimeIdentityUnavailable {
        version: String,
        source: PhysicsError,
    },
    #[error("MuJoCo runtime identity is missing native library hash or static build identity")]
    MissingNativeRuntimeIdentity,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn hash_model_dir(
    model_dir: impl AsRef<Path>,
    root_xml_relative_path: impl AsRef<Path>,
) -> Result<MujocoModelHash, ModelHashError> {
    let model_dir = model_dir.as_ref();
    let metadata = std::fs::symlink_metadata(model_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ModelHashError::InvalidModelDirectory {
            path: model_dir.to_path_buf(),
        });
    }

    let root_xml = normalize_reference_path(root_xml_relative_path.as_ref())?;
    let files = collect_model_files(model_dir)?;
    validate_reachable_xml_files(&root_xml, &files)?;

    let mut canonical = MODEL_HASH_DOMAIN.to_vec();
    for (path, bytes) in &files {
        append_canonical_file(&mut canonical, path.as_bytes(), bytes);
    }

    Ok(MujocoModelHash {
        hash_algorithm: HASH_ALGORITHM.to_string(),
        sha256_hex: sha256_hex(&canonical),
        root_xml_relative_path: PathBuf::from(root_xml),
    })
}

pub fn hash_embedded_model(xml_bytes: &[u8]) -> Result<MujocoModelHash, ModelHashError> {
    validate_xml_file_references(
        EMBEDDED_ROOT_XML,
        "",
        xml_bytes,
        &BTreeMap::new(),
        XmlReferenceMode::Embedded,
    )?;

    let mut canonical = EMBEDDED_MODEL_HASH_DOMAIN.to_vec();
    canonical.extend_from_slice(xml_bytes);

    Ok(MujocoModelHash {
        hash_algorithm: HASH_ALGORITHM.to_string(),
        sha256_hex: sha256_hex(&canonical),
        root_xml_relative_path: PathBuf::from(EMBEDDED_ROOT_XML),
    })
}

pub fn current_mujoco_runtime_identity() -> Result<MujocoRuntimeIdentity, ModelHashError> {
    let version = piper_physics::mujoco_runtime_version_string();
    let identity = piper_physics::loaded_mujoco_library_identity().map_err(|source| {
        ModelHashError::RuntimeIdentityUnavailable {
            version: version.clone(),
            source,
        }
    })?;

    if identity.native_library_sha256.is_none() && identity.static_build_identity.is_none() {
        return Err(ModelHashError::MissingNativeRuntimeIdentity);
    }

    Ok(MujocoRuntimeIdentity {
        version: Some(version),
        build_string: Some(identity.runtime_version),
        rust_binding_version: identity.rust_binding_version,
        native_library_sha256_hex: identity.native_library_sha256,
        static_build_identity: identity.static_build_identity,
    })
}

fn collect_model_files(model_dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, ModelHashError> {
    let mut files = BTreeMap::new();
    walk_model_dir(model_dir, model_dir, &mut files)?;
    Ok(files)
}

fn walk_model_dir(
    model_dir: &Path,
    current_dir: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), ModelHashError> {
    for entry in std::fs::read_dir(current_dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            return Err(ModelHashError::Symlink { path });
        }

        let relative_path = path
            .strip_prefix(model_dir)
            .expect("walked paths are always under the model directory");
        let normalized_path = normalize_filesystem_relative_path(relative_path)?;

        if metadata.is_dir() {
            walk_model_dir(model_dir, &path, files)?;
            continue;
        }

        if !metadata.is_file() {
            return Err(ModelHashError::NonRegularFile { path });
        }

        let bytes = std::fs::read(&path)?;
        if files.insert(normalized_path.clone(), bytes).is_some() {
            return Err(ModelHashError::DuplicateNormalizedPath {
                path: normalized_path,
            });
        }
    }

    Ok(())
}

fn normalize_filesystem_relative_path(path: &Path) -> Result<String, ModelHashError> {
    normalize_path_components(path, false)
}

fn normalize_reference_path(path: &Path) -> Result<String, ModelHashError> {
    validate_raw_relative_path(path)?;
    normalize_path_components(path, true)
}

fn normalize_path_components(
    path: &Path,
    reject_backslash_components: bool,
) -> Result<String, ModelHashError> {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Component::Normal(component) => {
                let component = component.to_str().ok_or_else(|| ModelHashError::NonUtf8Path {
                    path: path.to_path_buf(),
                })?;
                if component.is_empty()
                    || component == "."
                    || component == ".."
                    || (reject_backslash_components && component.contains('\\'))
                {
                    return Err(ModelHashError::InvalidRelativePath {
                        path: path.display().to_string(),
                    });
                }
                components.push(component);
            },
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ModelHashError::InvalidRelativePath {
                    path: path.display().to_string(),
                });
            },
        }
    }

    if components.is_empty() {
        return Err(ModelHashError::InvalidRelativePath {
            path: path.display().to_string(),
        });
    }

    let normalized = components.join("/");
    if normalized.contains('\\') {
        return Err(ModelHashError::InvalidRelativePath {
            path: path.display().to_string(),
        });
    }

    Ok(normalized)
}

fn validate_raw_relative_path(path: &Path) -> Result<(), ModelHashError> {
    let path = path.to_str().ok_or_else(|| ModelHashError::NonUtf8Path {
        path: path.to_path_buf(),
    })?;

    if path.is_empty() || path.contains('\\') {
        return Err(ModelHashError::InvalidRelativePath {
            path: path.to_string(),
        });
    }

    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(ModelHashError::InvalidRelativePath {
                path: path.to_string(),
            });
        }
    }

    Ok(())
}

fn append_canonical_file(out: &mut Vec<u8>, path: &[u8], content: &[u8]) {
    out.push(b'F');
    out.extend_from_slice(&(path.len() as u64).to_le_bytes());
    out.extend_from_slice(path);
    out.extend_from_slice(&(content.len() as u64).to_le_bytes());
    out.extend_from_slice(content);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XmlReferenceMode {
    Directory,
    Embedded,
}

fn validate_reachable_xml_files(
    root_xml: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), ModelHashError> {
    let mut pending = vec![root_xml.to_string()];
    let mut validated = BTreeSet::new();
    let root_xml_dir = root_xml_parent(root_xml);

    while let Some(xml_path) = pending.pop() {
        if !validated.insert(xml_path.clone()) {
            continue;
        }

        let bytes = files.get(&xml_path).ok_or_else(|| ModelHashError::RootXmlNotInModelTree {
            path: xml_path.clone(),
        })?;
        let includes = validate_xml_file_references(
            &xml_path,
            root_xml_dir,
            bytes,
            files,
            XmlReferenceMode::Directory,
        )?;
        pending.extend(includes);
    }

    Ok(())
}

fn validate_xml_file_references(
    xml_path: &str,
    root_xml_dir: &str,
    xml_bytes: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
    mode: XmlReferenceMode,
) -> Result<Vec<String>, ModelHashError> {
    let xml = std::str::from_utf8(xml_bytes).map_err(|_| ModelHashError::XmlNotUtf8 {
        path: xml_path.to_string(),
    })?;

    let mut position = 0;
    let mut saw_start_tag = false;
    let mut includes = Vec::new();
    while let Some(start_offset) = xml[position..].find('<') {
        let start = position + start_offset;
        let remaining = &xml[start..];

        if remaining.starts_with("<!--") {
            position = find_after(xml, start, "-->", xml_path)?;
            continue;
        }
        if remaining.starts_with("<![CDATA[") {
            position = find_after(xml, start, "]]>", xml_path)?;
            continue;
        }
        if remaining.starts_with("<?") {
            return Err(ModelHashError::InvalidXml {
                path: xml_path.to_string(),
                message: "unsupported XML processing instruction".to_string(),
            });
        }
        if remaining.starts_with("</") {
            position = find_after(xml, start, ">", xml_path)?;
            continue;
        }
        if remaining.starts_with("<!") {
            return Err(ModelHashError::InvalidXml {
                path: xml_path.to_string(),
                message: "unsupported XML declaration".to_string(),
            });
        }

        let tag_end = find_tag_end(xml, start + 1, xml_path)?;
        saw_start_tag = true;
        includes.extend(parse_start_tag_attributes(
            xml_path,
            root_xml_dir,
            &xml[start + 1..tag_end],
            files,
            mode,
        )?);
        position = tag_end + 1;
    }

    if !saw_start_tag {
        return Err(ModelHashError::InvalidXml {
            path: xml_path.to_string(),
            message: "no XML start tag found".to_string(),
        });
    }

    Ok(includes)
}

fn find_after(
    xml: &str,
    start: usize,
    delimiter: &str,
    xml_path: &str,
) -> Result<usize, ModelHashError> {
    xml[start..]
        .find(delimiter)
        .map(|offset| start + offset + delimiter.len())
        .ok_or_else(|| ModelHashError::InvalidXml {
            path: xml_path.to_string(),
            message: format!("missing `{delimiter}`"),
        })
}

fn find_tag_end(xml: &str, start: usize, xml_path: &str) -> Result<usize, ModelHashError> {
    let mut quote = None;
    for (offset, character) in xml[start..].char_indices() {
        match (quote, character) {
            (Some(current_quote), _) if character == current_quote => quote = None,
            (None, '"' | '\'') => quote = Some(character),
            (None, '>') => return Ok(start + offset),
            _ => {},
        }
    }

    Err(ModelHashError::InvalidXml {
        path: xml_path.to_string(),
        message: "unterminated start tag".to_string(),
    })
}

fn parse_start_tag_attributes(
    xml_path: &str,
    root_xml_dir: &str,
    tag: &str,
    files: &BTreeMap<String, Vec<u8>>,
    mode: XmlReferenceMode,
) -> Result<Vec<String>, ModelHashError> {
    let bytes = tag.as_bytes();
    let mut index = 0;

    skip_xml_whitespace(bytes, &mut index);
    let tag_name_start = index;
    while index < bytes.len() && !is_xml_whitespace(bytes[index]) && bytes[index] != b'/' {
        index += 1;
    }
    let tag_name = &tag[tag_name_start..index];
    if tag_name.is_empty() {
        return Err(ModelHashError::InvalidXml {
            path: xml_path.to_string(),
            message: "start tag is missing a name".to_string(),
        });
    }

    let mut includes = Vec::new();

    loop {
        skip_xml_whitespace(bytes, &mut index);
        if index >= bytes.len() || bytes[index] == b'/' {
            return Ok(includes);
        }

        let name_start = index;
        while index < bytes.len()
            && !is_xml_whitespace(bytes[index])
            && bytes[index] != b'='
            && bytes[index] != b'/'
        {
            index += 1;
        }
        let name = &tag[name_start..index];

        skip_xml_whitespace(bytes, &mut index);
        if index >= bytes.len() || bytes[index] != b'=' {
            return Err(ModelHashError::InvalidXml {
                path: xml_path.to_string(),
                message: format!("attribute `{name}` is missing a quoted value"),
            });
        }
        index += 1;
        skip_xml_whitespace(bytes, &mut index);

        if index >= bytes.len() || (bytes[index] != b'"' && bytes[index] != b'\'') {
            return Err(ModelHashError::InvalidXml {
                path: xml_path.to_string(),
                message: format!("attribute `{name}` is missing a quoted value"),
            });
        }
        let quote = bytes[index];
        index += 1;
        let value_start = index;
        while index < bytes.len() && bytes[index] != quote {
            index += 1;
        }
        if index >= bytes.len() {
            return Err(ModelHashError::InvalidXml {
                path: xml_path.to_string(),
                message: format!("attribute `{name}` has an unterminated value"),
            });
        }
        let value = &tag[value_start..index];
        index += 1;

        match name {
            "file" => {
                let resolved = validate_file_reference(xml_path, root_xml_dir, value, files, mode)?;
                if tag_name == "include" {
                    includes.push(resolved);
                }
            },
            "assetdir" | "meshdir" | "texturedir" | "strippath" => {
                return Err(ModelHashError::UnsupportedAssetResolutionAttribute {
                    xml_path: xml_path.to_string(),
                    attribute: name.to_string(),
                });
            },
            _ => {},
        }
    }
}

fn validate_file_reference(
    xml_path: &str,
    root_xml_dir: &str,
    reference: &str,
    files: &BTreeMap<String, Vec<u8>>,
    mode: XmlReferenceMode,
) -> Result<String, ModelHashError> {
    if mode == XmlReferenceMode::Embedded {
        return Err(ModelHashError::EmbeddedExternalFileReference {
            reference: reference.to_string(),
        });
    }

    if reference.is_empty()
        || reference.contains('&')
        || reference.contains('\0')
        || reference.contains('\\')
        || Path::new(reference).is_absolute()
    {
        return Err(ModelHashError::EscapingFileReference {
            xml_path: xml_path.to_string(),
            reference: reference.to_string(),
        });
    }

    let normalized_reference = normalize_reference_path(Path::new(reference)).map_err(|_| {
        ModelHashError::EscapingFileReference {
            xml_path: xml_path.to_string(),
            reference: reference.to_string(),
        }
    })?;
    let resolved = resolve_against_root_xml_dir(root_xml_dir, &normalized_reference);
    if !files.contains_key(&resolved) {
        return Err(ModelHashError::UnresolvedFileReference {
            xml_path: xml_path.to_string(),
            reference: reference.to_string(),
        });
    }

    Ok(resolved)
}

fn root_xml_parent(root_xml: &str) -> &str {
    match root_xml.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent,
        _ => "",
    }
}

fn resolve_against_root_xml_dir(root_xml_dir: &str, normalized_reference: &str) -> String {
    if root_xml_dir.is_empty() {
        normalized_reference.to_string()
    } else {
        format!("{root_xml_dir}/{normalized_reference}")
    }
}

fn skip_xml_whitespace(bytes: &[u8], index: &mut usize) {
    while *index < bytes.len() && is_xml_whitespace(bytes[*index]) {
        *index += 1;
    }
}

fn is_xml_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dir_hash_rejects_symlinks_and_non_utf8_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("piper_no_gripper.xml"), "<mujoco/>").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("piper_no_gripper.xml", dir.path().join("link.xml")).unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn embedded_model_hash_includes_domain_separator() {
        let hash = hash_embedded_model(b"<mujoco/>").unwrap();
        let direct = sha256_hex(b"<mujoco/>");
        assert_ne!(hash.sha256_hex, direct);
    }

    #[test]
    fn model_dir_hash_is_canonical_and_records_root_xml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<mujoco><asset><mesh file="assets/link.stl"/></asset></mujoco>"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("assets/link.stl"), b"mesh bytes").unwrap();

        let hash = hash_model_dir(dir.path(), "piper_no_gripper.xml").unwrap();
        let mut expected = b"PIPER_MUJOCO_MODEL_HASH_V1\n".to_vec();
        append_expected_file(&mut expected, "assets/link.stl", b"mesh bytes");
        append_expected_file(
            &mut expected,
            "piper_no_gripper.xml",
            br#"<mujoco><asset><mesh file="assets/link.stl"/></asset></mujoco>"#,
        );

        assert_eq!(hash.sha256_hex, sha256_hex(&expected));
        assert_eq!(
            hash.root_xml_relative_path,
            std::path::PathBuf::from("piper_no_gripper.xml")
        );
    }

    #[test]
    fn model_dir_hash_rejects_asset_paths_that_escape_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<mujoco><asset><mesh file="../outside.stl"/></asset></mujoco>"#,
        )
        .unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn model_dir_hash_validates_included_xml_regardless_of_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<mujoco><include file="nested.inc"/></mujoco>"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("nested.inc"), r#"<mesh file="/abs.stl"/>"#).unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn model_dir_hash_rejects_included_content_that_is_not_xml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<mujoco><include file="nested.inc"/></mujoco>"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("nested.inc"), b"not xml").unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn model_dir_hash_rejects_dot_components_in_root_xml_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/root.xml"), "<mujoco/>").unwrap();

        assert!(hash_model_dir(dir.path(), "sub/./root.xml").is_err());
        assert!(hash_model_dir(dir.path(), "sub/.").is_err());
    }

    #[test]
    fn model_dir_hash_rejects_dot_components_in_xml_file_references() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("assets")).unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<mujoco><asset><mesh file="assets/./mesh.stl"/></asset></mujoco>"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("assets/mesh.stl"), b"mesh bytes").unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn nested_xml_file_references_resolve_from_root_xml_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("models/nested")).unwrap();
        std::fs::create_dir_all(dir.path().join("models/assets")).unwrap();
        std::fs::write(
            dir.path().join("models/root.xml"),
            r#"<mujoco><include file="nested/nested.xml"/></mujoco>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("models/nested/nested.xml"),
            r#"<asset><mesh file="assets/link.stl"/></asset>"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("models/assets/link.stl"), b"mesh bytes").unwrap();

        hash_model_dir(dir.path(), "models/root.xml").unwrap();
    }

    #[test]
    fn nested_xml_file_references_reject_including_file_relative_resolution() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("models/nested/assets")).unwrap();
        std::fs::write(
            dir.path().join("models/root.xml"),
            r#"<mujoco><include file="nested/nested.xml"/></mujoco>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("models/nested/nested.xml"),
            r#"<asset><mesh file="assets/link.stl"/></asset>"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("models/nested/assets/link.stl"),
            b"mesh bytes",
        )
        .unwrap();

        assert!(hash_model_dir(dir.path(), "models/root.xml").is_err());
    }

    #[test]
    fn model_dir_hash_rejects_strippath_modifier() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<mujoco><compiler strippath="false"/></mujoco>"#,
        )
        .unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn model_dir_hash_rejects_unsupported_xml_declarations() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            "<!DOCTYPE mujoco><mujoco/>",
        )
        .unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn model_dir_hash_rejects_xml_processing_instructions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("piper_no_gripper.xml"),
            r#"<?xml version="1.0"?><mujoco/>"#,
        )
        .unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn reference_path_normalization_rejects_raw_backslashes() {
        assert!(normalize_reference_path(Path::new(r"assets\mesh.stl")).is_err());
    }

    #[test]
    fn filesystem_path_normalization_uses_canonical_separators() {
        let path = Path::new("models").join("assets").join("mesh.stl");
        assert_eq!(
            normalize_filesystem_relative_path(&path).unwrap(),
            "models/assets/mesh.stl"
        );
    }

    #[cfg(windows)]
    #[test]
    fn filesystem_path_normalization_accepts_windows_native_separators() {
        assert_eq!(
            normalize_filesystem_relative_path(Path::new(r"models\assets\mesh.stl")).unwrap(),
            "models/assets/mesh.stl"
        );
    }

    #[cfg(unix)]
    #[test]
    fn model_dir_hash_rejects_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("piper_no_gripper.xml"), "<mujoco/>").unwrap();
        std::fs::write(
            dir.path().join(std::ffi::OsString::from_vec(vec![0xff])),
            b"x",
        )
        .unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn model_dir_hash_rejects_empty_non_utf8_directories() {
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("piper_no_gripper.xml"), "<mujoco/>").unwrap();
        std::fs::create_dir(dir.path().join(std::ffi::OsString::from_vec(vec![0xff]))).unwrap();

        assert!(hash_model_dir(dir.path(), "piper_no_gripper.xml").is_err());
    }

    #[test]
    fn embedded_model_hash_rejects_external_file_references() {
        let err =
            hash_embedded_model(br#"<mujoco><include file="other.xml"/></mujoco>"#).unwrap_err();
        assert!(err.to_string().contains("external file reference"));
    }

    #[test]
    fn runtime_identity_wrapper_records_native_library_hash() {
        let identity = current_mujoco_runtime_identity().expect("runtime identity should resolve");

        assert!(identity.version.is_some());
        assert!(identity.build_string.is_some());
        assert_eq!(
            identity.native_library_sha256_hex.as_deref().map(str::len),
            Some(64)
        );
        assert!(identity.static_build_identity.is_none());
    }

    fn append_expected_file(out: &mut Vec<u8>, path: &str, bytes: &[u8]) {
        out.push(b'F');
        out.extend_from_slice(&(path.len() as u64).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
    }
}
