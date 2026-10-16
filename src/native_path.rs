//! JSON paths preserve ordinary strings and encode other native paths as file URIs.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, Serializer};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeUri {
    uri: lsp_types::Uri,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EncodedPath {
    Text(String),
    Native(NativeUri),
}

impl EncodedPath {
    fn decode(self) -> Result<PathBuf, &'static str> {
        match self {
            Self::Text(path) => Ok(path.into()),
            Self::Native(value) => {
                // Do not let URL normalization turn file:relative into an
                // absolute path. The native encoder always emits file://.
                if !value
                    .uri
                    .as_str()
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("file://"))
                {
                    return Err("Native path object must contain an absolute file URI");
                }
                tex_ls_protocol::uri_to_fs_path(&value.uri)
                    .filter(|path| path.is_absolute())
                    .ok_or("Native path object must contain an absolute file URI")
            }
        }
    }
}

fn serialize_path<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    if let Some(path) = path.to_str() {
        serializer.serialize_str(path)
    } else {
        let uri = tex_ls_protocol::path_to_uri(path).ok_or_else(|| {
            serde::ser::Error::custom("Cannot represent native path as an absolute file URI")
        })?;
        NativeUri { uri }.serialize(serializer)
    }
}

pub(crate) mod path {
    use super::*;
    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        serialize_path(path, serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        EncodedPath::deserialize(deserializer)?
            .decode()
            .map_err(serde::de::Error::custom)
    }
}

pub(crate) mod paths {
    use super::*;
    use serde::ser::SerializeSeq;
    struct PathRef<'a>(&'a Path);
    impl Serialize for PathRef<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serialize_path(self.0, serializer)
        }
    }
    pub fn serialize<S: Serializer>(paths: &[PathBuf], serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(paths.len()))?;
        for path in paths {
            sequence.serialize_element(&PathRef(path))?;
        }
        sequence.end()
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<PathBuf>, D::Error> {
        Vec::<EncodedPath>::deserialize(deserializer)?
            .into_iter()
            .map(|path| path.decode().map_err(serde::de::Error::custom))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Fixture {
        #[serde(with = "path")]
        path: PathBuf,
        #[serde(with = "paths")]
        roots: Vec<PathBuf>,
    }

    #[test]
    fn ordinary_strings_keep_literal_uri_looking_paths() {
        let value =
            serde_json::json!({"path": "file:///literal", "roots": ["relative", "/ordinary"]});
        let fixture: Fixture = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(fixture.path, Path::new("file:///literal"));
        assert_eq!(serde_json::to_value(fixture).unwrap(), value);
    }

    #[test]
    fn invalid_native_objects_are_rejected() {
        for path in [
            serde_json::json!({"uri": "https://example.com/file"}),
            serde_json::json!({"uri": "relative.tex"}),
            serde_json::json!({"uri": "file:relative.tex"}),
            serde_json::json!({"uri": "file:///valid", "other": true}),
            serde_json::json!({"uri": "file:///bad%00path"}),
            serde_json::json!({"uri": "file:///valid?query=true"}),
        ] {
            assert!(
                serde_json::from_value::<Fixture>(serde_json::json!({"path": path, "roots": []}))
                    .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn exact_native_paths_roundtrip_and_relative_native_paths_fail() {
        use std::os::unix::ffi::OsStringExt;
        let native = PathBuf::from(std::ffi::OsString::from_vec(
            b"/project/main\xff.tex".to_vec(),
        ));
        let fixture = Fixture {
            path: native.clone(),
            roots: vec!["/ordinary".into(), native],
        };
        let value = serde_json::to_value(&fixture).unwrap();
        assert!(value["path"]["uri"].is_string());
        assert_eq!(value["roots"][0], "/ordinary");
        assert_eq!(serde_json::from_value::<Fixture>(value).unwrap(), fixture);
        let relative = Fixture {
            path: PathBuf::from(std::ffi::OsString::from_vec(b"relative\xff".to_vec())),
            roots: vec![],
        };
        assert!(serde_json::to_value(relative).is_err());
    }
}
