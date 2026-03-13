use std::{collections::HashMap, path::PathBuf};

use eyre::Context;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{MANIFEST_NAME, VERSION};

const STD_GIT_URL: &str = "git@github.com:benjaminjellis/mond-std.git";
const STD_GIT_TAG: &str = "0.0.1";

#[derive(Serialize, Deserialize)]
pub(crate) struct MondManifest {
    pub(crate) package: Package,
    pub(crate) dependencies: HashMap<String, DependencySpec>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Package {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) mond_version: Version,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DependencySpec {
    pub(crate) git: String,
    #[serde(flatten)]
    pub(crate) reference: GitReference,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum GitReference {
    #[serde(rename = "tag")]
    Tag(String),
    #[serde(rename = "branch")]
    Branch(String),
    #[serde(rename = "rev")]
    Rev(String),
}

impl MondManifest {
    fn new(name: String) -> Self {
        let dependencies = HashMap::from([(
            "std".to_string(),
            DependencySpec {
                git: STD_GIT_URL.to_string(),
                reference: GitReference::Tag(STD_GIT_TAG.to_string()),
            },
        )]);
        Self {
            package: Package {
                name,
                version: Version::new(0, 1, 0),
                mond_version: Version::parse(VERSION).unwrap(),
            },
            dependencies,
        }
    }
}

pub(crate) fn read_manifest(root: PathBuf) -> eyre::Result<MondManifest> {
    let manifest_file_path = root.join(MANIFEST_NAME);
    let file = std::fs::read(&manifest_file_path).context(format!(
        "failed to read {MANIFEST_NAME} at {manifest_file_path:?}"
    ))?;
    let manifest: MondManifest =
        toml::from_slice(&file).context(format!("failed to parse {MANIFEST_NAME}"))?;
    Ok(manifest)
}

pub(crate) fn create_new_manifest(name: String) -> MondManifest {
    MondManifest::new(name)
}

pub(crate) fn write_manifest(manifest: &MondManifest, path: &PathBuf) -> eyre::Result<()> {
    let mut manifest_as_string = String::new();
    manifest_as_string.push_str("[package]\n");
    manifest_as_string.push_str(&format!(
        "name = {}\n",
        toml::Value::String(manifest.package.name.clone())
    ));
    manifest_as_string.push_str(&format!(
        "version = {}\n",
        toml::Value::String(manifest.package.version.to_string())
    ));
    manifest_as_string.push_str(&format!(
        "mond_version = {}\n",
        toml::Value::String(manifest.package.mond_version.to_string())
    ));
    manifest_as_string.push('\n');
    manifest_as_string.push_str("[dependencies]\n");

    let mut dep_names: Vec<&String> = manifest.dependencies.keys().collect();
    dep_names.sort();
    for dep_name in dep_names {
        let dep = &manifest.dependencies[dep_name];
        let (ref_key, ref_val) = match &dep.reference {
            GitReference::Tag(v) => ("tag", v),
            GitReference::Branch(v) => ("branch", v),
            GitReference::Rev(v) => ("rev", v),
        };
        manifest_as_string.push_str(&format!(
            "{dep_name} = {{ git = {}, {ref_key} = {} }}\n",
            toml::Value::String(dep.git.clone()),
            toml::Value::String(ref_val.clone())
        ));
    }

    std::fs::write(path, manifest_as_string).context("failed to write {MANIFEST_NAME} to disk")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_file() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mond-manifest-test-{}-{nanos}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn write_manifest_uses_inline_dependency_entries() {
        let path = unique_temp_file();
        let manifest = create_new_manifest("app".to_string());
        write_manifest(&manifest, &path).expect("write manifest");
        let written = std::fs::read_to_string(&path).expect("read manifest");

        assert!(
            written.contains("[dependencies]"),
            "missing [dependencies] section: {written}"
        );
        assert!(
            written.contains("std = { git = "),
            "std dependency should be inline in [dependencies]: {written}"
        );
        assert!(
            !written.contains("[dependencies.std]"),
            "manifest should avoid [dependencies.std] table form: {written}"
        );

        let parsed: MondManifest = toml::from_str(&written).expect("parse written manifest");
        assert!(parsed.dependencies.contains_key("std"));

        let _ = std::fs::remove_file(path);
    }
}
