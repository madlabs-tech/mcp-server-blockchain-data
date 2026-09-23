use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// A secret that never prints or serializes its value. Use [`Redacted::expose`] at the edge
/// (building a request); never store the exposed value in logs, errors or responses.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub fn new(v: T) -> Self {
        Self(v)
    }

    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"***\"")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T> Serialize for Redacted<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("***")
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Redacted<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self)
    }
}

/// Replace any occurrence of `secret` inside `text` (e.g. a URL with the key in its path).
pub(crate) fn scrub(text: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|s| s.len() >= 6)
        .fold(text.to_owned(), |acc, s| acc.replace(s, "***"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_leaks() {
        let r = Redacted::new("sk_live_123456".to_string());
        assert_eq!(format!("{r:?}"), "\"***\"");
        assert_eq!(format!("{r}"), "***");
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"***\"");
        assert_eq!(r.expose(), "sk_live_123456");
        let back: Redacted<String> = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(back.expose(), "abc");
    }

    #[test]
    fn scrubs_urls() {
        let url = "https://eth-mainnet.g.alchemy.com/v2/abcdef123456";
        assert_eq!(
            scrub(url, &["abcdef123456"]),
            "https://eth-mainnet.g.alchemy.com/v2/***"
        );
    }
}
