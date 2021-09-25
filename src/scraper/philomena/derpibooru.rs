use std::str::FromStr;

use anyhow::Result;
use log::trace;
use ref_thread_local::{ref_thread_local, RefThreadLocal};
use regex::Regex;
use reqwest::Url;

ref_thread_local! {
    static managed URL_REGEX: Regex = Regex::from_str(r#"^https://derpibooru.org/(images/)?(?P<image_id>\d+).*$"#).expect("failure in setting up essential regex");
}

pub async fn is_derpibooru(url: &Url) -> Result<bool> {
    if URL_REGEX.borrow().is_match(url.as_str()) {
        trace!("derpibooru matched on URL pattern 1");
        return Ok(true);
    }
    trace!("derpibooru didn't match on pattern");
    Ok(false)
}

pub fn url_to_api(url: &Url) -> Result<Option<Url>> {
    for cap in URL_REGEX.borrow().captures(url.as_str()) {
        match cap.name("image_id") {
            None => return Ok(None),
            Some(m) => {
                let url = format!("https://derpibooru.org/api/v1/json/images/{}", m.as_str());
                return Ok(Some(Url::from_str(&url)?));
            }
        }
    }
    anyhow::bail!("did not match derpibooru URL")
}
