// The formatting of one character: bold, italic, and an optional link.
//
// Every character in a document carries one of these, stored next to
// the text. Plain text is just the default: nothing bold, nothing
// italic, no link. Keeping the formatting per character, instead of
// as separate ranges, is what makes concurrent formatting converge:
// a format op sets the attributes of the characters it covers, and
// an insert that lands inside a formatted range picks those
// attributes up, so both orders of applying the two ops agree.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Attrs {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub link: Option<String>,
}

impl Attrs {
    pub fn plain() -> Self {
        Self::default()
    }

    pub fn bold() -> Self {
        Self {
            bold: true,
            ..Default::default()
        }
    }

    pub fn italic() -> Self {
        Self {
            italic: true,
            ..Default::default()
        }
    }

    pub fn link(url: impl Into<String>) -> Self {
        Self {
            link: Some(url.into()),
            ..Default::default()
        }
    }

    pub fn is_plain(&self) -> bool {
        !self.bold && !self.italic && self.link.is_none()
    }
}

/// A change to apply to the formatting of a range. Each field is
/// `None` when that attribute is left alone: toggling bold off a range
/// that is also italic must not clear the italic, and adding a link
/// must not touch the bold.
//
// The `link` field is an option inside an option, on purpose: `None`
// leaves the link alone, `Some(None)` removes it, `Some(Some(url))`
// sets it. Plain derive would read an explicit JSON `null` as the
// outer `None`, so the field has a custom deserializer: a missing
// field leaves the link alone, an explicit `null` removes it, and a
// string sets it. Serialization skips a missing field, so the two
// `None`s never collide in JSON either.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FormatChange {
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_link_change"
    )]
    pub link: Option<Option<String>>,
}

/// Reads the `link` field of a format change, telling a missing field
/// apart from an explicit null: missing means leave the link alone,
/// null means remove it, a string means set it.
fn de_link_change<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct LinkChange;

    impl<'de> serde::de::Visitor<'de> for LinkChange {
        type Value = Option<Option<String>>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a link, null to remove it, or nothing to leave it alone")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Some(None))
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Some(None))
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            String::deserialize(deserializer).map(|url| Some(Some(url)))
        }
    }

    deserializer.deserialize_option(LinkChange)
}

impl FormatChange {
    pub fn bold(bold: bool) -> Self {
        Self {
            bold: Some(bold),
            ..Default::default()
        }
    }

    pub fn italic(italic: bool) -> Self {
        Self {
            italic: Some(italic),
            ..Default::default()
        }
    }

    pub fn link(url: Option<String>) -> Self {
        Self {
            link: Some(url),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bold.is_none() && self.italic.is_none() && self.link.is_none()
    }

    /// The attributes a character ends up with after this change.
    pub fn apply_to(&self, attrs: &Attrs) -> Attrs {
        Attrs {
            bold: self.bold.unwrap_or(attrs.bold),
            italic: self.italic.unwrap_or(attrs.italic),
            link: self.link.clone().unwrap_or_else(|| attrs.link.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_leaves_unspecified_attributes_alone() {
        let attrs = Attrs {
            bold: true,
            italic: true,
            link: Some("https://example.com".into()),
        };
        assert_eq!(
            FormatChange::bold(false).apply_to(&attrs),
            Attrs {
                bold: false,
                italic: true,
                link: Some("https://example.com".into()),
            }
        );
        assert_eq!(
            FormatChange::link(None).apply_to(&attrs),
            Attrs {
                bold: true,
                italic: true,
                link: None,
            }
        );
        assert_eq!(FormatChange::default().apply_to(&attrs), attrs);
    }

    #[test]
    fn link_change_round_trips_through_json() {
        // Removing a link serializes as an explicit null; leaving it
        // alone omits the field, so the two never get confused.
        let remove = FormatChange::link(None);
        assert_eq!(
            serde_json::to_string(&remove).unwrap(),
            r#"{"bold":null,"italic":null,"link":null}"#
        );
        let leave = FormatChange::bold(true);
        assert_eq!(
            serde_json::to_string(&leave).unwrap(),
            r#"{"bold":true,"italic":null}"#
        );
        let back: FormatChange = serde_json::from_str(r#"{"link":null}"#).unwrap();
        assert_eq!(back, FormatChange::link(None));
        let back: FormatChange = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(back, FormatChange::default());
    }
}
