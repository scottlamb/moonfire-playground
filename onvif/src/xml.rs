/// A wrapper which escapes text as suitable in XML PCDATA.
/// That is, regular text between elements, rather than within an attribute.
pub struct EscapedText<'a>(pub &'a str);

impl<'a> std::fmt::Display for EscapedText<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut prev = 0;
        for p in memchr::memchr2_iter(b'<', b'&', self.0.as_bytes()) {
            f.write_str(&self.0[prev..p])?;
            match self.0.as_bytes()[p] {
                b'<' => f.write_str("&lt;")?,
                b'&' => f.write_str("&amp;")?,
                _ => unreachable!(),
            }
            prev = p + 1; // skip over the '<' or '&'.
        }
        f.write_str(&self.0[prev..])
    }
}

pub fn find<'a, 'd>(
    n: roxmltree::Node<'a, 'd>,
    path: &[(&str, &str)],
) -> Option<roxmltree::Node<'a, 'd>> {
    let mut cur = n;
    'outer: for i in path {
        for child in cur.children() {
            if child.has_tag_name(*i) {
                cur = child;
                continue 'outer;
            }
        }
        return None;
    }
    Some(cur)
}

#[cfg(test)]
mod tests {
    #[test]
    fn escape() {
        use super::EscapedText;
        assert_eq!("", EscapedText("").to_string());
        assert_eq!("foo", EscapedText("foo").to_string());
        assert_eq!("&lt;foo", EscapedText("<foo").to_string());
        assert_eq!("&lt;foo&amp;", EscapedText("<foo&").to_string());
        assert_eq!("&lt;foo&amp;bar", EscapedText("<foo&bar").to_string());
    }

    #[test]
    fn find() {
        use super::find;
        let doc =
            roxmltree::Document::parse(include_str!("testdata/get_capabilities_response.xml"))
                .unwrap();
        const NS_SOAP: &'static str = "http://www.w3.org/2003/05/soap-envelope";
        const NS_DEVICE: &'static str = "http://www.onvif.org/ver10/device/wsdl";
        const NS_ONVIF: &'static str = "http://www.onvif.org/ver10/schema";
        assert_eq!(
            find(
                doc.root(),
                &[
                    (NS_SOAP, "Envelope"),
                    (NS_SOAP, "Body"),
                    (NS_DEVICE, "GetCapabilitiesResponse"),
                    (NS_DEVICE, "Capabilities"),
                    (NS_ONVIF, "Events"),
                    (NS_ONVIF, "XAddr"),
                ]
            )
            .unwrap()
            .tag_name(),
            (NS_ONVIF, "XAddr").into()
        );
        assert_eq!(find(doc.root(), &[(NS_SOAP, "Body")]), None);
        assert_eq!(find(doc.root(), &[(NS_DEVICE, "Envelope")]), None);
        assert_eq!(find(doc.root(), &[]), Some(doc.root()));
    }
}
