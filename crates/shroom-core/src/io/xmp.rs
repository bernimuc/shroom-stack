//! Adapting a source frame's XMP packet for a stacked result, and recording the frames a
//! result was stacked from (as `xmpMM:Ingredients`, the XMP property for the documents a
//! composite is made of).

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

static INGREDIENTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\s*<xmpMM:Ingredients>.*?</xmpMM:Ingredients>").unwrap());
/// The description holding only ingredients, as written by [`with_ingredients`].
static OWN_DESCRIPTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<rdf:Description rdf:about="" xmlns:xmpMM="[^"]*" xmlns:stRef="[^"]*">\s*<xmpMM:Ingredients>.*?</xmpMM:Ingredients>\s*</rdf:Description>"#).unwrap()
});
/// A file path of a resource reference, as element or attribute.
static FILE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"stRef:filePath(?:="([^"]*)"|>([^<]*)</stRef:filePath>)"#).unwrap());

/// Returns the XMP for a new file derived from `xmp`: crop settings removed and a fresh
/// document identity, so catalogues don't confuse the result with the source frame.
/// Everything else (labels, lens, develop settings, history) is kept.
pub(super) fn for_result(xmp: &str, uuid: &str) -> String {
    let xmp = CROP_ATTR.replace_all(xmp, "");
    let xmp = CROP_ELEM.replace_all(&xmp, "");
    let xmp = DOCUMENT_ID.replace_all(&xmp, |c: &regex::Captures| format!("{}xmp.did:{uuid}", &c[1]));
    INSTANCE_ID.replace_all(&xmp, |c: &regex::Captures| format!("{}xmp.iid:{uuid}", &c[1])).into_owned()
}

/// Adds `names` (the source frames' file names) as ingredients to `xmp`, replacing any
/// ingredients it has; creates a packet if there is none. `None` if there is nothing to write.
pub(super) fn with_ingredients(xmp: Option<&str>, names: &[String]) -> Option<String> {
    if names.is_empty() {
        return xmp.map(str::to_owned);
    }
    let items: String = names
        .iter()
        .map(|n| format!("<rdf:li rdf:parseType=\"Resource\"><stRef:filePath>{}</stRef:filePath></rdf:li>", escape(n)))
        .collect();
    let description = format!(
        "<rdf:Description rdf:about=\"\" xmlns:xmpMM=\"http://ns.adobe.com/xap/1.0/mm/\" \
         xmlns:stRef=\"http://ns.adobe.com/xap/1.0/sType/ResourceRef#\">\
         <xmpMM:Ingredients><rdf:Bag>{items}</rdf:Bag></xmpMM:Ingredients></rdf:Description>"
    );
    let xmp = xmp.map(|x| OWN_DESCRIPTION.replace_all(x, "").into_owned());
    match xmp.as_deref().map(|x| INGREDIENTS.replace_all(x, "")) {
        Some(x) if x.contains("</rdf:RDF>") => Some(x.replacen("</rdf:RDF>", &format!("{description}</rdf:RDF>"), 1)),
        // Without an RDF block there is nowhere to add a description; keep the packet.
        Some(x) => Some(x.into_owned()),
        None => Some(format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
             {description}</rdf:RDF></x:xmpmeta>"
        )),
    }
}

/// File names of the ingredients recorded in `xmp`.
pub(super) fn ingredients(xmp: &str) -> Vec<String> {
    let Some(block) = INGREDIENTS.find(xmp) else { return Vec::new() };
    FILE_PATH
        .captures_iter(block.as_str())
        .filter_map(|c| c.get(1).or(c.get(2)))
        .map(|m| unescape(m.as_str()))
        .collect()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
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
        for kept in [r#"xmp:Label="Blau""#, r#"crs:Exposure2012="+0.50""#, r#"crs:HasSettings="True""#] {
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
        assert_eq!(for_result(xmp, "x"), "<rdf:Description>\n <crs:Tint>+3</crs:Tint>\n</rdf:Description>");
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

    #[test]
    fn ingredients_roundtrip() {
        let names = vec!["IMG_1.dng".to_string(), "A & B.CR3".to_string()];
        let out = with_ingredients(Some(ATTR_XMP), &names);
        // ATTR_XMP has no RDF block, so it is kept unchanged.
        assert_eq!(out.as_deref(), Some(ATTR_XMP));

        let packet = with_ingredients(None, &names).unwrap();
        assert_eq!(ingredients(&packet), names);
        // Replacing: a second call does not duplicate the ingredients.
        let again = with_ingredients(Some(&packet), &names[..1]).unwrap();
        assert_eq!(ingredients(&again), &names[..1]);
        assert_eq!(again.matches("<rdf:Description").count(), 1, "{again}");

        let full = format!("<x:xmpmeta><rdf:RDF>{ATTR_XMP}</rdf:RDF></x:xmpmeta>");
        let out = with_ingredients(Some(&full), &names).unwrap();
        assert!(out.contains(r#"xmp:Label="Blau""#));
        assert_eq!(ingredients(&out), names);
        assert_eq!(with_ingredients(None, &[]), None);
    }

    #[test]
    fn reads_ingredients_as_attributes() {
        let xmp = r#"<xmpMM:Ingredients><rdf:Bag><rdf:li stRef:filePath="a.dng"/><rdf:li stRef:filePath="b.dng"/></rdf:Bag></xmpMM:Ingredients>"#;
        assert_eq!(ingredients(xmp), vec!["a.dng", "b.dng"]);
        assert!(ingredients("<rdf:RDF/>").is_empty());
    }
}
