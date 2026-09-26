use super::LinkError;

/// The `key=value&…` tail of a link, percent-decoded. First occurrence of a
/// key wins.
pub(super) struct Query(Vec<(String, String)>);

impl Query {
    pub(super) fn parse(s: &str) -> Self {
        Query(
            s.split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                    (percent_decode(k), percent_decode(v))
                })
                .collect(),
        )
    }

    /// The value under `key`, if present and non-empty.
    pub(super) fn optional(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    pub(super) fn required(&self, key: &'static str) -> Result<&str, LinkError> {
        self.optional(key).ok_or(LinkError::MissingParam(key))
    }
}

/// Undo `encodeURIComponent`: every `%XX` becomes its byte. `+` is left
/// alone — the producers here are URL encoders, not HTML forms, and a `+`
/// inside a URL is a `+`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%2Fb%3Fc%3D1"), "a/b?c=1");
        assert_eq!(percent_decode("plus+stays"), "plus+stays");
        assert_eq!(percent_decode("trailing%2"), "trailing%2");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%C3%A9"), "é");
    }
}
