//! Adapting a source frame's XMP packet for a stacked result.

use std::sync::LazyLock;

use regex::Regex;

/// Camera Raw crop settings, as attributes (`crs:CropTop="0.1"`) or elements
/// (`<crs:CropTop>0.1</crs:CropTop>`). The result has a different geometry than the
/// source frame, so any crop is reset.
static CROP_ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\s+crs:(?:HasCrop|Crop\w*)="[^"]*""#).unwrap());
static CROP_ELEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*<crs:(?:HasCrop|Crop\w*)>[^<]*</crs:(?:HasCrop|Crop\w*)>").unwrap());
static DOCUMENT_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(xmpMM:DocumentID="|<xmpMM:DocumentID>)[^"<]*"#).unwrap());
static INSTANCE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(xmpMM:InstanceID="|<xmpMM:InstanceID>)[^"<]*"#).unwrap());

/// Returns the XMP for a new file derived from `xmp`: crop settings removed and a fresh
/// document identity, so catalogues don't confuse the result with the source frame.
/// Everything else (labels, lens, develop settings, history) is kept.
pub(super) fn for_result(xmp: &str, uuid: &str) -> String {
    let xmp = CROP_ATTR.replace_all(xmp, "");
    let xmp = CROP_ELEM.replace_all(&xmp, "");
    let xmp = DOCUMENT_ID.replace_all(&xmp, |c: &regex::Captures| format!("{}xmp.did:{uuid}", &c[1]));
    INSTANCE_ID
        .replace_all(&xmp, |c: &regex::Captures| format!("{}xmp.iid:{uuid}", &c[1]))
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTR_XMP: &str = r#"<rdf:Description rdf:about=""
   xmp:Label="Blau"
   xmpMM:DocumentID="xmp.did:ba76c478"
   xmpMM:OriginalDocumentID="443D9754"
   xmpMM:InstanceID="xmp.iid:ba76c478"
   crs:Exposure2012="+0.50"
   crs:CropTop="0.1"
   crs:CropLeft="0.05"
   crs:CropBottom="0.9"
   crs:CropRight="0.95"
   crs:CropAngle="1.2"
   crs:CropConstrainToWarp="0"
   crs:HasSettings="True"
   crs:HasCrop="True"
   crs:AlreadyApplied="False">
   <xmpMM:History/>
  </rdf:Description>"#;

    #[test]
    fn removes_crop_attributes_only() {
        let out = for_result(ATTR_XMP, "new");
        assert!(!out.contains("Crop"), "{out}");
        for kept in [
            r#"xmp:Label="Blau""#,
            r#"crs:Exposure2012="+0.50""#,
            r#"crs:HasSettings="True""#,
        ] {
            assert!(out.contains(kept), "missing {kept}: {out}");
        }
        assert!(out.ends_with(
            r#"crs:AlreadyApplied="False">
   <xmpMM:History/>
  </rdf:Description>"#
        ));
    }

    #[test]
    fn removes_crop_elements() {
        let xmp = "<rdf:Description>\n <crs:CropTop>0.1</crs:CropTop>\n <crs:HasCrop>True</crs:HasCrop>\n <crs:Tint>+3</crs:Tint>\n</rdf:Description>";
        assert_eq!(
            for_result(xmp, "x"),
            "<rdf:Description>\n <crs:Tint>+3</crs:Tint>\n</rdf:Description>"
        );
    }

    #[test]
    fn replaces_document_identity_but_keeps_original() {
        let out = for_result(ATTR_XMP, "1234");
        assert!(out.contains(r#"xmpMM:DocumentID="xmp.did:1234""#), "{out}");
        assert!(out.contains(r#"xmpMM:InstanceID="xmp.iid:1234""#), "{out}");
        assert!(out.contains(r#"xmpMM:OriginalDocumentID="443D9754""#), "{out}");

        let elem = "<xmpMM:DocumentID>xmp.did:old</xmpMM:DocumentID>";
        assert_eq!(for_result(elem, "n"), "<xmpMM:DocumentID>xmp.did:n</xmpMM:DocumentID>");
    }
}
