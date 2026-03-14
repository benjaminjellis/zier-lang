use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use eyre::Context;
use semver::Version;
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value, value};

use crate::MANIFEST_NAME;

const STD_GIT_URL: &str = "git@github.com:benjaminjellis/mond-std.git";
const STD_GIT_TAG: &str = "0.0.4";

#[derive(Serialize, Deserialize)]
pub(crate) struct BahnManifest {
    pub(crate) package: Package,
    pub(crate) dependencies: HashMap<String, DependencySpec>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Package {
    pub(crate) name: String,
    pub(crate) version: Version,
    pub(crate) min_mond_version: Option<Version>,
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

impl BahnManifest {
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
                min_mond_version: None,
            },
            dependencies,
        }
    }
}

pub(crate) fn read_manifest(root: PathBuf) -> eyre::Result<BahnManifest> {
    let manifest_file_path = root.join(MANIFEST_NAME);
    let file = std::fs::read(&manifest_file_path).context(format!(
        "failed to read {MANIFEST_NAME} at {manifest_file_path:?}"
    ))?;
    let manifest: BahnManifest =
        toml::from_slice(&file).context(format!("failed to parse {MANIFEST_NAME}"))?;
    Ok(manifest)
}

pub(crate) fn create_new_manifest(name: String) -> BahnManifest {
    BahnManifest::new(name)
}

pub(crate) fn write_manifest(manifest: &BahnManifest, path: &PathBuf) -> eyre::Result<()> {
    let mut doc = DocumentMut::new();
    let mut package_table = Table::new();
    package_table["name"] = value(manifest.package.name.clone());
    package_table["version"] = value(manifest.package.version.to_string());
    if let Some(min_mond_version) = &manifest.package.min_mond_version {
        package_table["min_mond_version"] = value(min_mond_version.to_string());
    }
    doc["package"] = Item::Table(package_table);

    let mut dependencies_table = Table::new();

    let dependencies = manifest.dependencies.iter().collect::<BTreeMap<_, _>>();
    for (dep_name, dep) in dependencies {
        let mut inline = InlineTable::new();
        inline.insert("git", Value::from(dep.git.clone()));
        match &dep.reference {
            GitReference::Tag(tag) => inline.insert("tag", Value::from(tag.clone())),
            GitReference::Branch(branch) => inline.insert("branch", Value::from(branch.clone())),
            GitReference::Rev(rev) => inline.insert("rev", Value::from(rev.clone())),
        };
        inline.fmt();
        dependencies_table[dep_name] = Item::Value(Value::InlineTable(inline));
    }
    doc["dependencies"] = Item::Table(dependencies_table);

    let manifest_as_string = doc.to_string();
    std::fs::write(path, manifest_as_string)
        .context(format!("failed to write {MANIFEST_NAME} to disk"))?;
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
            "bahn-manifest-test-{}-{nanos}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn write_manifest_serializes_dependencies_as_inline_entries() {
        let path = unique_temp_file();
        let manifest = create_new_manifest("app".to_string());
        write_manifest(&manifest, &path).expect("write manifest");
        let written = std::fs::read_to_string(&path).expect("read manifest");

        assert!(
            written.contains("[dependencies]"),
            "missing [dependencies] section: {written}"
        );
        assert!(
            written.contains("std = {"),
            "manifest should use inline dependency table form: {written}"
        );

        let parsed: BahnManifest = toml::from_str(&written).expect("parse written manifest");
        assert!(parsed.dependencies.contains_key("std"));

        let _ = std::fs::remove_file(path);
    }
}
